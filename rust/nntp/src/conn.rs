//! One NNTP connection as a single-task state machine.
//!
//! Requests arrive from a shared queue and up to `inflight` are on the wire at once. A reader task
//! turns the socket into `(code, text)` responses; the state machine matches them to requests in
//! order. `POST` is two-phase (RFC 3977): after `POST` is sent nothing else may be sent until the
//! `340` arrives and the body has been written, otherwise the server would read the next command
//! as article text. `STAT` can be pipelined freely.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;

use crate::throttle::Throttle;
use crate::{Endpoint, NntpError, tls};

pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type BoxStream = Pin<Box<dyn Stream>>;

/// Tracks window usage across a provider's connections so new connections are opened only when
/// every existing window is full (otherwise idle workers would grab sequential requests and open
/// a connection each).
#[derive(Default)]
pub(crate) struct Scaler {
    capacity: AtomicUsize,
    used: AtomicUsize,
    pub(crate) notify: Notify,
}

impl Scaler {
    pub(crate) fn add_capacity(&self, n: usize) {
        self.capacity.fetch_add(n, Ordering::AcqRel);
    }
    #[allow(deprecated)] // `try_update` on newer toolchains
    pub(crate) fn remove_capacity(&self, n: usize) {
        let _ = self
            .capacity
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                Some(c.saturating_sub(n))
            });
    }
    pub(crate) fn add_used(&self, n: usize) {
        self.used.fetch_add(n, Ordering::AcqRel);
        if self.needs_more() {
            self.notify.notify_waiters();
        }
    }
    #[allow(deprecated)] // `try_update` on newer toolchains
    pub(crate) fn remove_used(&self, n: usize) {
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                Some(c.saturating_sub(n))
            });
    }
    /// True when no window slot is free.
    pub(crate) fn needs_more(&self) -> bool {
        self.used.load(Ordering::Acquire) >= self.capacity.load(Ordering::Acquire)
    }
}

/// What a request wants.
pub(crate) enum Kind {
    /// Headers, blank line and yEnc body; the terminating `.` line is added when sending.
    Post(Vec<u8>),
    Stat(String),
}

pub(crate) enum Answer {
    Posted,
    Present(bool),
}

pub(crate) struct Request {
    pub kind: Kind,
    pub reply: oneshot::Sender<Result<Answer, NntpError>>,
}

/// Why a connection's loop ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// Nothing to do for `idle_timeout`.
    Idle,
    /// Reached its time-to-live and drained.
    Ttl,
    /// The connection broke (all pending requests were failed).
    Failed,
    /// The shared queue was closed.
    QueueClosed,
}

#[derive(Clone)]
pub(crate) struct ConnCfg {
    pub inflight: usize,
    pub idle_timeout: Duration,
    pub ttl: Duration,
    /// Longest silence tolerated while requests are outstanding.
    pub stall_timeout: Duration,
}

struct Response {
    code: u16,
    text: String,
}

/// An authenticated session, ready to run.
pub(crate) struct Session {
    reader: BufReader<ReadHalf<BoxStream>>,
    writer: WriteHalf<BoxStream>,
}

async fn read_response(
    r: &mut BufReader<ReadHalf<BoxStream>>,
    line: &mut String,
) -> Result<Response, NntpError> {
    line.clear();
    match r.read_line(line).await {
        Ok(0) | Err(_) => Err(NntpError::Disconnected),
        Ok(_) => {
            let t = line.trim_end();
            let code = t
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .ok_or_else(|| NntpError::Protocol {
                    code: 0,
                    text: t.to_string(),
                })?;
            Ok(Response {
                code,
                text: t.get(4..).unwrap_or("").to_string(),
            })
        }
    }
}

async fn dial(ep: &Endpoint) -> Result<BoxStream, NntpError> {
    let target = (ep.host.as_str(), ep.port);
    let tcp: TcpStream = match &ep.proxy {
        Some(p) => {
            let s = match &p.auth {
                Some((u, pw)) => {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(
                        p.addr.as_str(),
                        target,
                        u,
                        pw,
                    )
                    .await
                }
                None => tokio_socks::tcp::Socks5Stream::connect(p.addr.as_str(), target).await,
            }
            .map_err(|e| NntpError::Connect(format!("failed to connect via proxy: {e}")))?;
            s.into_inner()
        }
        None => TcpStream::connect(target)
            .await
            .map_err(|e| NntpError::Connect(e.to_string()))?,
    };
    let _ = tcp.set_nodelay(true);
    if !ep.tls {
        return Ok(Box::pin(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(ep.host.clone())
        .map_err(|e| NntpError::Connect(e.to_string()))?;
    let s = tokio_rustls::TlsConnector::from(tls::client_config(ep.insecure_tls))
        .connect(name, tcp)
        .await
        .map_err(|e| NntpError::Connect(format!("TLS handshake failed: {e}")))?;
    Ok(Box::pin(s))
}

/// Opens a connection: dial (SOCKS5 first when configured, then TLS), greeting, optional AUTHINFO.
pub(crate) async fn open(ep: &Endpoint) -> Result<Session, NntpError> {
    let session = tokio::time::timeout(ep.connect_timeout, async {
        let stream = dial(ep).await?;
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::with_capacity(16 * 1024, r);
        let mut line = String::new();
        let greeting = read_response(&mut r, &mut line).await?;
        if !matches!(greeting.code, 200 | 201) {
            return Err(NntpError::Connect(format!(
                "unexpected greeting: {} {}",
                greeting.code, greeting.text
            )));
        }
        if !ep.username.is_empty() {
            let io = |_| NntpError::Disconnected;
            w.write_all(format!("AUTHINFO USER {}\r\n", ep.username).as_bytes())
                .await
                .map_err(io)?;
            let user = read_response(&mut r, &mut line).await?;
            match user.code {
                281 => {}
                381 => {
                    w.write_all(format!("AUTHINFO PASS {}\r\n", ep.password).as_bytes())
                        .await
                        .map_err(io)?;
                    let pass = read_response(&mut r, &mut line).await?;
                    if pass.code != 281 {
                        return Err(NntpError::Auth {
                            code: pass.code,
                            text: pass.text,
                        });
                    }
                }
                code => {
                    return Err(NntpError::Auth {
                        code,
                        text: user.text,
                    });
                }
            }
        }
        Ok(Session {
            reader: r,
            writer: w,
        })
    })
    .await
    .map_err(|_| NntpError::Timeout)??;
    Ok(session)
}

enum Pending {
    Stat(oneshot::Sender<Result<Answer, NntpError>>),
    /// `POST` sent; the body is written when `340` arrives.
    AwaitGo(Vec<u8>, oneshot::Sender<Result<Answer, NntpError>>),
    /// Body sent; waiting for the final status.
    AwaitResult(oneshot::Sender<Result<Answer, NntpError>>),
}

fn fail(pending: &mut VecDeque<Pending>, err: &NntpError, scaler: &Scaler) {
    scaler.remove_used(pending.len());
    for p in pending.drain(..) {
        let tx = match p {
            Pending::Stat(t) | Pending::AwaitResult(t) | Pending::AwaitGo(_, t) => t,
        };
        let _ = tx.send(Err(err.clone()));
    }
}

/// Serves requests from `queue` on `session` until it ends (see [`End`]).
pub(crate) async fn run(
    session: Session,
    queue: &async_channel::Receiver<Request>,
    cfg: &ConnCfg,
    first: Option<Request>,
    throttle: Option<Arc<Throttle>>,
    scaler: &Scaler,
) -> End {
    let Session {
        mut reader,
        mut writer,
    } = session;
    let (tx, mut responses) = mpsc::channel::<Result<Response, NntpError>>(1024);
    let reader_task = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            let r = read_response(&mut reader, &mut line).await;
            let stop = r.is_err();
            if tx.send(r).await.is_err() || stop {
                break;
            }
        }
    });

    let started = Instant::now();
    let mut pending: VecDeque<Pending> = VecDeque::new();
    let mut last_activity = Instant::now();
    let mut next_request = first;
    let end = 'outer: loop {
        let blocked = pending.iter().any(|p| matches!(p, Pending::AwaitGo(..)));
        let draining = started.elapsed() >= cfg.ttl;
        let can_take = !blocked && !draining && pending.len() < cfg.inflight;

        // a request taken but not yet sent (the first one handed in by the pool)
        if can_take && let Some(req) = next_request.take() {
            if send(&mut writer, &mut pending, req).await.is_err() {
                fail(&mut pending, &NntpError::Disconnected, scaler);
                break End::Failed;
            }
            continue;
        }
        if draining && pending.is_empty() {
            break End::Ttl;
        }

        let idle_deadline = last_activity + cfg.idle_timeout;
        let stall_deadline = last_activity + cfg.stall_timeout;
        tokio::select! {
            biased;
            resp = responses.recv() => {
                last_activity = Instant::now();
                let resp = match resp {
                    Some(Ok(r)) => r,
                    _ => {
                        fail(&mut pending, &NntpError::Disconnected, scaler);
                        break 'outer End::Failed;
                    }
                };
                if resp.code == 400 || resp.code == 205 || pending.is_empty() {
                    // server is closing, or an answer nobody asked for: the stream is out of sync
                    fail(&mut pending, &NntpError::Disconnected, scaler);
                    break 'outer End::Failed;
                }
                if !handle(resp, &mut pending, &mut writer, &throttle, scaler).await {
                    break 'outer End::Failed;
                }
            }
            req = queue.recv(), if can_take => {
                match req {
                    Ok(r) => {
                        last_activity = Instant::now();
                        scaler.add_used(1);
                        next_request = Some(r);
                    }
                    Err(_) => {
                        if pending.is_empty() {
                            break 'outer End::QueueClosed;
                        }
                        // finish what is in flight, then stop
                        let r = drain_responses(&mut responses, &mut pending, &mut writer, &throttle, scaler).await;
                        break 'outer if r { End::QueueClosed } else { End::Failed };
                    }
                }
            }
            () = tokio::time::sleep_until(idle_deadline), if pending.is_empty() => break 'outer End::Idle,
            () = tokio::time::sleep_until(stall_deadline), if !pending.is_empty() => {
                fail(&mut pending, &NntpError::Timeout, scaler);
                break 'outer End::Failed;
            }
            () = tokio::time::sleep_until(started + cfg.ttl), if !draining => {}
        }
    };
    reader_task.abort();
    if let Some(req) = next_request.take() {
        scaler.remove_used(1);
        let _ = req.reply.send(Err(NntpError::Disconnected));
    }
    let _ = writer.shutdown().await;
    end
}

/// Writes the command for `req` and records what to expect.
async fn send(
    w: &mut WriteHalf<BoxStream>,
    pending: &mut VecDeque<Pending>,
    req: Request,
) -> std::io::Result<()> {
    match req.kind {
        Kind::Stat(id) => {
            w.write_all(format!("STAT <{}>\r\n", id.trim_matches(['<', '>'])).as_bytes())
                .await?;
            w.flush().await?;
            pending.push_back(Pending::Stat(req.reply));
        }
        Kind::Post(body) => {
            w.write_all(b"POST\r\n").await?;
            w.flush().await?;
            pending.push_back(Pending::AwaitGo(body, req.reply));
        }
    }
    Ok(())
}

/// Matches one response to the oldest pending request. Returns false when the connection broke.
async fn handle(
    resp: Response,
    pending: &mut VecDeque<Pending>,
    writer: &mut WriteHalf<BoxStream>,
    throttle: &Option<Arc<Throttle>>,
    scaler: &Scaler,
) -> bool {
    match pending.pop_front() {
        Some(Pending::Stat(reply)) => {
            let _ = reply.send(match resp.code {
                223 => Ok(Answer::Present(true)),
                430 => Ok(Answer::Present(false)),
                code => Err(NntpError::Protocol {
                    code,
                    text: resp.text,
                }),
            });
        }
        Some(Pending::AwaitGo(body, reply)) if resp.code == 340 => {
            if let Some(t) = throttle {
                t.acquire(body.len() as u64).await;
            }
            if writer.write_all(&body).await.is_err()
                || writer.write_all(b".\r\n").await.is_err()
                || writer.flush().await.is_err()
            {
                scaler.remove_used(1);
                let _ = reply.send(Err(NntpError::Disconnected));
                fail(pending, &NntpError::Disconnected, scaler);
                return false;
            }
            pending.push_front(Pending::AwaitResult(reply));
        }
        Some(Pending::AwaitGo(_, reply)) => {
            let _ = reply.send(Err(NntpError::PostRejected {
                code: resp.code,
                text: resp.text,
            }));
        }
        Some(Pending::AwaitResult(reply)) => {
            let _ = reply.send(if resp.code == 240 {
                Ok(Answer::Posted)
            } else {
                Err(NntpError::PostRejected {
                    code: resp.code,
                    text: resp.text,
                })
            });
        }
        None => {}
    }
    true
}

/// After the queue closed: answers still arriving for in-flight requests are processed, then stop.
async fn drain_responses(
    responses: &mut mpsc::Receiver<Result<Response, NntpError>>,
    pending: &mut VecDeque<Pending>,
    writer: &mut WriteHalf<BoxStream>,
    throttle: &Option<Arc<Throttle>>,
    scaler: &Scaler,
) -> bool {
    while !pending.is_empty() {
        let Some(Ok(resp)) = responses.recv().await else {
            fail(pending, &NntpError::Disconnected, scaler);
            return false;
        };
        if !handle(resp, pending, writer, throttle, scaler).await {
            return false;
        }
    }
    true
}
