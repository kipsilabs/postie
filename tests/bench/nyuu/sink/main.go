// nntp-sink runs the javi11/nntp-server-mock protocol handler with an
// in-memory backend that discards article bodies, so uploader benchmarks
// measure the client rather than the server's disk.
package main

import (
	"bufio"
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"math/big"
	"net"
	"os"
	"os/signal"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/javi11/nntp-server-mock/nntpserver"
)

type sink struct {
	articles atomic.Int64
	bytes    atomic.Int64
	group    *nntpserver.Group
}

func (s *sink) ListGroups(int) ([]*nntpserver.Group, error) { return []*nntpserver.Group{s.group}, nil }
func (s *sink) GetGroup(string) (*nntpserver.Group, error)  { return s.group, nil }
func (s *sink) GetArticle(*nntpserver.Group, string) (*nntpserver.Article, error) {
	return nil, errors.New("430 no such article")
}
func (s *sink) GetArticles(*nntpserver.Group, int64, int64) ([]nntpserver.NumberedArticle, error) {
	return nil, nil
}
func (s *sink) Authorized() bool                                        { return true }
func (s *sink) Authenticate(string, string) (nntpserver.Backend, error) { return nil, nil }
func (s *sink) AllowPost() bool                                         { return true }
func (s *sink) Post(a *nntpserver.Article) error {
	n, err := io.Copy(io.Discard, a.Body)
	if err != nil {
		return err
	}
	s.articles.Add(1)
	s.bytes.Add(n)
	return nil
}
func (s *sink) Stat(_ *nntpserver.Group, id string) (string, string, error) { return "0", id, nil }
func (s *sink) Date() time.Time                                             { return time.Now() }

func selfSigned() tls.Certificate {
	key, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "127.0.0.1"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(24 * time.Hour),
		IPAddresses:  []net.IP{net.ParseIP("127.0.0.1")},
	}
	der, _ := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

func main() {
	addr := flag.String("addr", "127.0.0.1:1199", "listen address")
	useTLS := flag.Bool("tls", false, "serve TLS with a self-signed certificate")
	fast := flag.Bool("fast", false, "bypass the mock's textproto parser and drain POST bodies with a raw reader")
	flag.Parse()
	log.SetOutput(io.Discard) // the mock logs every command; that would dominate the benchmark

	b := &sink{group: &nntpserver.Group{Name: "alt.binaries.test", Posting: nntpserver.PostingPermitted}}
	srv := nntpserver.NewServer(b)

	ln, err := net.Listen("tcp", *addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if *useTLS {
		ln = tls.NewListener(ln, &tls.Config{Certificates: []tls.Certificate{selfSigned()}})
	}
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			if *fast {
				go b.fastProcess(c)
				continue
			}
			go srv.Process(c)
		}
	}()
	fmt.Printf("sink listening on %s tls=%v\n", ln.Addr(), *useTLS)

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	_ = ln.Close()
	fmt.Printf("sink: articles=%d bytes=%d\n", b.articles.Load(), b.bytes.Load())
}

var dotLine = []byte(".\r\n")

// fastProcess speaks just enough NNTP for an uploader and drains POST bodies
// with bufio.ReadSlice, so the server side costs ~nothing and the client is
// what gets measured. The mock's textproto path tops out around 1 GiB/s.
func (s *sink) fastProcess(conn net.Conn) {
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
		case strings.HasPrefix(cmd, "MODE"):
			_, _ = w.WriteString("200 posting ok\r\n")
		case cmd == "POST":
			_, _ = w.WriteString("340 go\r\n")
			_ = w.Flush()
			n, err := drain(r)
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

func drain(r *bufio.Reader) (int64, error) {
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
