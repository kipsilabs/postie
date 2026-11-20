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
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter, ReadHalf,
    WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;

use crate::throttle::Throttle;
use crate::{Endpoint, NntpError, tls};

pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type BoxStream = Pin<Box<dyn Stream>>;
type Writer = BufWriter<WriteHalf<BoxStream>>;

/// Bounds one write/flush so a peer that stops reading cannot wedge the connection task.
async fn bounded<F>(limit: Duration, f: F) -> std::io::Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    match tokio::time::timeout(limit, f).await {
        Ok(r) => r,
        Err(_) => Err(std::io::ErrorKind::TimedOut.into()),
    }
}

/// Tracks how many requests a provider's connections can still accept right now, so new
/// connections are opened only when every existing one is full (otherwise idle workers would grab
/// sequential requests and open a connection each). A connection that is blocked mid-`POST`
/// (waiting for the `340`) or draining contributes nothing, so POST traffic fans out across
/// connections instead of funnelling through one that cannot take another request.
#[derive(Default)]
pub(crate) struct Scaler {
    avail: AtomicUsize,
    pub(crate) notify: Notify,
}

impl Scaler {
    pub(crate) fn add_avail(&self, n: usize) {
        self.avail.fetch_add(n, Ordering::AcqRel);
    }
    #[allow(deprecated)] // `try_update` on newer toolchains
    pub(crate) fn remove_avail(&self, n: usize) {
        let _ = self
            .avail
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                Some(c.saturating_sub(n))
            });
        if self.needs_more() {
            self.notify.notify_waiters();
        }
    }
    /// Moves one connection's published availability from `*published` to `want`.
    fn publish(&self, published: &mut usize, want: usize) {
        if want > *published {
            self.add_avail(want - *published);
        } else if want < *published {
            self.remove_avail(*published - want);
        }
        *published = want;
    }
    /// True when no connection can take another request.
    pub(crate) fn needs_more(&self) -> bool {
        self.avail.load(Ordering::Acquire) == 0
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
    // detect half-dead peers (NAT drops, unplugged cables) without waiting for a write to fail
    let _ = socket2::SockRef::from(&tcp).set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(60))
            .with_interval(Duration::from_secs(20)),
    );
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

fn fail(pending: &mut VecDeque<Pending>, err: &NntpError) {
    for p in pending.drain(..) {
        let tx = match p {
            Pending::Stat(t) | Pending::AwaitResult(t) | Pending::AwaitGo(_, t) => t,
        };
        let _ = tx.send(Err(err.clone()));
    }
}

/// Serves requests from `queue` on `session` until it ends (see [`End`]).
///
/// The caller has already reserved `inflight - 1` of this connection's slots in `scaler` (the
/// first request takes the other one); the connection keeps its published availability current and
/// releases it all when it ends.
pub(crate) async fn run(
    session: Session,
    queue: &async_channel::Receiver<Request>,
    cfg: &ConnCfg,
    first: Option<Request>,
    throttle: Option<Arc<Throttle>>,
    scaler: &Scaler,
) -> End {
    let Session { mut reader, writer } = session;
    let mut writer: Writer = BufWriter::with_capacity(16 * 1024, writer);
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
    let mut published = cfg.inflight.saturating_sub(1);
    // commands written to the buffer but not yet flushed to the socket
    let mut dirty = false;
    let end = 'outer: loop {
        let blocked = pending.iter().any(|p| matches!(p, Pending::AwaitGo(..)));
        let draining = started.elapsed() >= cfg.ttl;
        let can_take = !blocked && !draining && pending.len() < cfg.inflight;

        // pull whatever is already queued without waiting, so several STATs share one flush
        if can_take && next_request.is_none() && dirty {
            next_request = queue.try_recv().ok();
        }
        // a request taken but not yet sent (the first one handed in by the pool)
        if can_take && let Some(req) = next_request.take() {
            if send(&mut writer, &mut pending, req, cfg.stall_timeout)
                .await
                .is_err()
            {
                fail(&mut pending, &NntpError::Disconnected);
                break End::Failed;
            }
            dirty = true;
            continue;
        }
        if dirty {
            if bounded(cfg.stall_timeout, writer.flush()).await.is_err() {
                fail(&mut pending, &NntpError::Disconnected);
                break End::Failed;
            }
            dirty = false;
        }
        if draining && pending.is_empty() {
            break End::Ttl;
        }
        let want = if blocked || draining {
            0
        } else {
            cfg.inflight
                .saturating_sub(pending.len() + usize::from(next_request.is_some()))
        };
        scaler.publish(&mut published, want);

        let idle_deadline = last_activity + cfg.idle_timeout;
        let stall_deadline = last_activity + cfg.stall_timeout;
        tokio::select! {
            biased;
            resp = responses.recv() => {
                last_activity = Instant::now();
                let resp = match resp {
                    Some(Ok(r)) => r,
                    _ => {
                        fail(&mut pending, &NntpError::Disconnected);
                        break 'outer End::Failed;
                    }
                };
                if resp.code == 400 || resp.code == 205 || pending.is_empty() {
                    // server is closing, or an answer nobody asked for: the stream is out of sync
                    fail(&mut pending, &NntpError::Disconnected);
                    break 'outer End::Failed;
                }
                if !handle(resp, &mut pending, &mut writer, &throttle, cfg.stall_timeout).await {
                    break 'outer End::Failed;
                }
            }
            req = queue.recv(), if can_take => {
                match req {
                    Ok(r) => {
                        last_activity = Instant::now();
                        next_request = Some(r);
                    }
                    Err(_) => {
                        if pending.is_empty() {
                            break 'outer End::QueueClosed;
                        }
                        // finish what is in flight, then stop
                        let r = drain_responses(&mut responses, &mut pending, &mut writer, &throttle, cfg.stall_timeout).await;
                        break 'outer if r { End::QueueClosed } else { End::Failed };
                    }
                }
            }
            () = tokio::time::sleep_until(idle_deadline), if pending.is_empty() => break 'outer End::Idle,
            () = tokio::time::sleep_until(stall_deadline), if !pending.is_empty() => {
                fail(&mut pending, &NntpError::Timeout);
                break 'outer End::Failed;
            }
            () = tokio::time::sleep_until(started + cfg.ttl), if !draining => {}
        }
    };
    scaler.publish(&mut published, 0);
    reader_task.abort();
    if let Some(req) = next_request.take() {
        let _ = req.reply.send(Err(NntpError::Disconnected));
    }
    let _ = bounded(cfg.stall_timeout, writer.shutdown()).await;
    end
}

/// Buffers the command for `req` and records what to expect. `POST` is flushed at once (nothing
/// else may be sent until its `340`); `STAT`s are left for the caller to flush in a batch.
async fn send(
    w: &mut Writer,
    pending: &mut VecDeque<Pending>,
    req: Request,
    limit: Duration,
) -> std::io::Result<()> {
    match req.kind {
        Kind::Stat(id) => {
            let cmd = format!("STAT <{}>\r\n", id.trim_matches(['<', '>']));
            bounded(limit, w.write_all(cmd.as_bytes())).await?;
            pending.push_back(Pending::Stat(req.reply));
        }
        Kind::Post(body) => {
            bounded(limit, async {
                w.write_all(b"POST\r\n").await?;
                w.flush().await
            })
            .await?;
            pending.push_back(Pending::AwaitGo(body, req.reply));
        }
    }
    Ok(())
}

/// Matches one response to the oldest pending request. Returns false when the connection broke.
async fn handle(
    resp: Response,
    pending: &mut VecDeque<Pending>,
    writer: &mut Writer,
    throttle: &Option<Arc<Throttle>>,
    limit: Duration,
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
            let wrote = bounded(limit, async {
                writer.write_all(&body).await?;
                writer.write_all(b".\r\n").await?;
                writer.flush().await
            })
            .await;
            if wrote.is_err() {
                let _ = reply.send(Err(NntpError::Disconnected));
                fail(pending, &NntpError::Disconnected);
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
/// Silence for longer than `limit` fails what is left.
async fn drain_responses(
    responses: &mut mpsc::Receiver<Result<Response, NntpError>>,
    pending: &mut VecDeque<Pending>,
    writer: &mut Writer,
    throttle: &Option<Arc<Throttle>>,
    limit: Duration,
) -> bool {
    while !pending.is_empty() {
        let Ok(Some(Ok(resp))) = tokio::time::timeout(limit, responses.recv()).await else {
            fail(pending, &NntpError::Disconnected);
            return false;
        };
        if !handle(resp, pending, writer, throttle, limit).await {
            return false;
        }
    }
    true
}
