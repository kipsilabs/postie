package postie

import (
	"bufio"
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"io"
	"log/slog"
	"math/big"
	mrand "math/rand/v2"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/par2"
	"github.com/javi11/postie/internal/pool"
	"github.com/javi11/postie/internal/progress"
	"github.com/javi11/postie/pkg/fileinfo"
)

// Pipeline benchmarks: PAR2 creation → yEnc → NNTP POST against an in-process
// sink server, so the numbers isolate Postie's own CPU/IO work from the network.
//
//	go test ./pkg/postie -run '^$' -bench 'Pipeline' -benchtime 1x -count 3
//
// Environment knobs:
//
//	POSTIE_BENCH_MB            input size in MiB (default 256)
//	POSTIE_BENCH_CONNS         fake-server connections (default 20)
//	POSTIE_BENCH_TLS=1         serve the sink over TLS (as real providers do)
//	POSTIE_BENCH_PAR2_THREADS  par2 GF16 compute threads (default 0 = NumCPU)

type sinkNNTPServer struct {
	ln       net.Listener
	articles atomic.Int64
	bytes    atomic.Int64
}

func startSinkNNTPServer(tb testing.TB, useTLS bool) *sinkNNTPServer {
	tb.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		tb.Fatalf("listen: %v", err)
	}
	if useTLS {
		ln = tls.NewListener(ln, &tls.Config{Certificates: []tls.Certificate{selfSignedCert(tb)}})
	}
	s := &sinkNNTPServer{ln: ln}
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go s.handle(c)
		}
	}()
	tb.Cleanup(func() { _ = ln.Close() })
	return s
}

func (s *sinkNNTPServer) port() int { return s.ln.Addr().(*net.TCPAddr).Port }

func selfSignedCert(tb testing.TB) tls.Certificate {
	tb.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		tb.Fatal(err)
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "127.0.0.1"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		IPAddresses:  []net.IP{net.ParseIP("127.0.0.1")},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		tb.Fatal(err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

// noopProgress satisfies progress.JobProgress/Progress without rendering
// terminal progress bars, which would otherwise dominate benchmark output.
type noopProgress struct{ id uuid.UUID }

func (noopProgress) AddProgress(id uuid.UUID, _ string, _ progress.ProgressType, _ int64) progress.Progress {
	return noopProgress{id: id}
}
func (noopProgress) FinishProgress(uuid.UUID)                        {}
func (noopProgress) GetProgress(uuid.UUID) progress.Progress         { return nil }
func (noopProgress) GetAllProgress() map[uuid.UUID]progress.Progress { return nil }
func (noopProgress) GetAllProgressState() []progress.ProgressState   { return nil }
func (noopProgress) GetJobID() string                                { return "bench" }
func (noopProgress) Close()                                          {}
func (noopProgress) SetAllPaused(bool)                               {}
func (noopProgress) UpdateProgress(int64)                            {}
func (noopProgress) Finish()                                         {}
func (noopProgress) GetState() progress.ProgressState                { return progress.ProgressState{} }
func (n noopProgress) GetID() uuid.UUID                              { return n.id }
func (noopProgress) GetName() string                                 { return "" }
func (noopProgress) GetType() progress.ProgressType                  { return progress.ProgressTypeUploading }
func (noopProgress) GetCurrent() int64                               { return 0 }
func (noopProgress) GetTotal() int64                                 { return 0 }
func (noopProgress) GetPercentage() float64                          { return 0 }
func (noopProgress) IsComplete() bool                                { return false }
func (noopProgress) GetStartTime() time.Time                         { return time.Time{} }
func (noopProgress) GetElapsedTime() time.Duration                   { return 0 }
func (noopProgress) SetPaused(bool)                                  {}
func (noopProgress) IsPaused() bool                                  { return false }
func (noopProgress) SetWaitDeadline(time.Time)                       {}

var dotLine = []byte(".\r\n")

func (s *sinkNNTPServer) handle(conn net.Conn) {
	defer func() { _ = conn.Close() }()
	w := bufio.NewWriter(conn)
	r := bufio.NewReaderSize(conn, 1<<20)
	_, _ = w.WriteString("200 sink ready\r\n")
	_ = w.Flush()
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		cmd := strings.ToUpper(strings.TrimSpace(line))
		switch {
		case cmd == "":
		case strings.HasPrefix(cmd, "AUTHINFO USER"):
			_, _ = w.WriteString("381 pass\r\n")
		case strings.HasPrefix(cmd, "AUTHINFO PASS"):
			_, _ = w.WriteString("281 ok\r\n")
		case cmd == "CAPABILITIES":
			_, _ = w.WriteString("101 caps\r\nVERSION 2\r\nREADER\r\nPOST\r\nDATE\r\n.\r\n")
		case cmd == "DATE":
			_, _ = w.WriteString("111 " + time.Now().UTC().Format("20060102150405") + "\r\n")
		case cmd == "POST":
			_, _ = w.WriteString("340 go\r\n")
			_ = w.Flush()
			n, err := drainArticle(r)
			if err != nil {
				return
			}
			s.articles.Add(1)
			s.bytes.Add(n)
			_, _ = w.WriteString("240 posted\r\n")
		case strings.HasPrefix(cmd, "STAT "):
			_, _ = w.WriteString("223 0 " + strings.TrimSpace(line[5:]) + "\r\n")
		case cmd == "QUIT":
			_, _ = w.WriteString("205 bye\r\n")
			_ = w.Flush()
			return
		default:
			_, _ = w.WriteString("500 unknown\r\n")
		}
		if err := w.Flush(); err != nil {
			return
		}
	}
}

// drainArticle consumes the article body up to and including the lone "."
// terminator line. ReadSlice reuses the reader's buffer, so this is
// allocation-free for the ~6000 yEnc lines of a 750 KiB article.
func drainArticle(r *bufio.Reader) (int64, error) {
	var total int64
	for {
		line, err := r.ReadSlice('\n')
		for err == bufio.ErrBufferFull {
			total += int64(len(line))
			line, err = r.ReadSlice('\n')
		}
		if err != nil {
			return total, err
		}
		total += int64(len(line))
		if bytes.Equal(line, dotLine) {
			return total, nil
		}
	}
}

func benchEnvInt(name string, def int) int {
	if v := os.Getenv(name); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return n
		}
	}
	return def
}

// writeBenchFile writes sizeMB MiB of incompressible pseudo-random data.
func writeBenchFile(tb testing.TB, dir, name string, sizeMB int) fileinfo.FileInfo {
	tb.Helper()
	path := filepath.Join(dir, name)
	f, err := os.Create(path)
	if err != nil {
		tb.Fatalf("create: %v", err)
	}
	rng := mrand.NewChaCha8([32]byte{1, 2, 3})
	buf := make([]byte, 1<<20)
	w := bufio.NewWriterSize(f, 4<<20)
	for range sizeMB {
		_, _ = rng.Read(buf)
		if _, err := w.Write(buf); err != nil {
			tb.Fatalf("write: %v", err)
		}
	}
	if err := w.Flush(); err != nil {
		tb.Fatalf("flush: %v", err)
	}
	if err := f.Close(); err != nil {
		tb.Fatalf("close: %v", err)
	}
	return fileinfo.FileInfo{Path: path, Size: uint64(sizeMB) << 20}
}

func benchConfig(port, conns int, par2Enabled, waitForPar2 bool, tempDir string) *config.ConfigData {
	cfg := config.GetDefaultConfig()
	enabled := true
	useTLS := os.Getenv("POSTIE_BENCH_TLS") == "1"
	cfg.Servers = []config.ServerConfig{{
		Host:           "127.0.0.1",
		Port:           port,
		MaxConnections: conns,
		Enabled:        &enabled,
		Role:           config.ServerRoleUpload,
		SSL:            useTLS,
		InsecureSSL:    useTLS,
	}}
	cfg.Posting.WaitForPar2 = &waitForPar2
	cfg.Par2.Enabled = &par2Enabled
	cfg.Par2.TempDir = tempDir
	cfg.Par2.NumGoroutines = benchEnvInt("POSTIE_BENCH_PAR2_THREADS", 0)
	disabled := false
	cfg.PostCheck.Enabled = &disabled
	return &cfg
}

type benchRun struct {
	name        string
	par2Enabled bool
	waitForPar2 bool
	folder      bool
}

func BenchmarkPipeline(b *testing.B) {
	slog.SetDefault(slog.New(slog.NewTextHandler(io.Discard, nil)))
	sizeMB := benchEnvInt("POSTIE_BENCH_MB", 256)
	conns := benchEnvInt("POSTIE_BENCH_CONNS", 20)
	useTLS := os.Getenv("POSTIE_BENCH_TLS") == "1"

	srcDir := b.TempDir()
	file := writeBenchFile(b, srcDir, "payload.bin", sizeMB)

	b.Run("par2-only", func(b *testing.B) {
		cfg := benchConfig(1, conns, true, true, b.TempDir())
		exec := par2.NewExecutor(cfg.Posting.ArticleSizeInBytes, &cfg.Par2, nil)
		b.SetBytes(int64(file.Size))
		b.ResetTimer()
		for range b.N {
			outDir := b.TempDir()
			if _, err := exec.CreateInDirectory(context.Background(), []fileinfo.FileInfo{file}, outDir); err != nil {
				b.Fatal(err)
			}
		}
	})

	runs := []benchRun{
		{name: "upload-only", par2Enabled: false, waitForPar2: true},
		{name: "full/wait_for_par2", par2Enabled: true, waitForPar2: true},
		{name: "full/parallel_par2", par2Enabled: true, waitForPar2: false},
		{name: "folder/wait_for_par2", par2Enabled: true, waitForPar2: true, folder: true},
		{name: "folder/parallel_par2", par2Enabled: true, waitForPar2: false, folder: true},
	}
	for _, run := range runs {
		b.Run(run.name, func(b *testing.B) {
			srv := startSinkNNTPServer(b, useTLS)
			cfg := benchConfig(srv.port(), conns, run.par2Enabled, run.waitForPar2, b.TempDir())
			pm, err := pool.New(cfg)
			if err != nil {
				b.Fatal(err)
			}
			b.Cleanup(func() { _ = pm.Close() })

			b.SetBytes(int64(file.Size))
			b.ResetTimer()
			for range b.N {
				b.StopTimer()
				outDir := b.TempDir()
				p, err := New(context.Background(), cfg, pm, noopProgress{}, nil)
				if err != nil {
					b.Fatal(err)
				}
				files := []fileinfo.FileInfo{file}
				rootDir := srcDir
				if run.folder {
					rootDir = filepath.Dir(srcDir)
				}
				b.StartTimer()
				start := time.Now()
				if _, err := p.Post(context.Background(), files, rootDir, outDir, run.folder); err != nil {
					b.Fatal(err)
				}
				el := time.Since(start)
				b.StopTimer()
				p.Close()
				b.ReportMetric(float64(file.Size)/(1<<20)/el.Seconds(), "MiB/s")
				b.StartTimer()
			}
			b.ReportMetric(float64(srv.articles.Load())/float64(b.N), "articles/op")
		})
	}
}
