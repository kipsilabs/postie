package wshub

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func newTestHub(t *testing.T, ping, pongWait time.Duration) (*Hub, *httptest.Server) {
	t.Helper()
	hub := NewHub()
	hub.pingInterval = ping
	hub.pongWait = pongWait
	go hub.Run()
	srv := httptest.NewServer(http.HandlerFunc(hub.ServeHTTP))
	t.Cleanup(srv.Close)
	return hub, srv
}

func dial(t *testing.T, srv *httptest.Server) *websocket.Conn {
	t.Helper()
	url := "ws" + strings.TrimPrefix(srv.URL, "http")
	conn, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func TestHubSendsPingsToConnectedClient(t *testing.T) {
	_, srv := newTestHub(t, 20*time.Millisecond, time.Second)
	conn := dial(t, srv)

	pinged := make(chan struct{}, 1)
	conn.SetPingHandler(func(string) error {
		select {
		case pinged <- struct{}{}:
		default:
		}
		return nil
	})
	go func() {
		for {
			if _, _, err := conn.ReadMessage(); err != nil {
				return
			}
		}
	}()

	select {
	case <-pinged:
	case <-time.After(time.Second):
		t.Fatal("server never sent a ping frame")
	}
}

func TestHubClosesClientThatStopsAnsweringPings(t *testing.T) {
	_, srv := newTestHub(t, 20*time.Millisecond, 100*time.Millisecond)
	conn := dial(t, srv)

	// A half-dead peer: frames still arrive but nothing is ever answered.
	conn.SetPingHandler(func(string) error { return nil })

	closed := make(chan error, 1)
	go func() {
		for {
			if _, _, err := conn.ReadMessage(); err != nil {
				closed <- err
				return
			}
		}
	}()

	select {
	case <-closed:
	case <-time.After(2 * time.Second):
		t.Fatal("server kept a client that never answered pings")
	}
}

func TestHubDeliversEventsAfterReconnect(t *testing.T) {
	hub, srv := newTestHub(t, time.Hour, time.Hour)
	first := dial(t, srv)
	_ = first.Close()

	second := dial(t, srv)
	got := make(chan string, 1)
	go func() {
		_, msg, err := second.ReadMessage()
		if err == nil {
			got <- string(msg)
		}
	}()

	deadline := time.After(2 * time.Second)
	for {
		hub.EmitEvent("nntp-pool-metrics-updated", map[string]int{"n": 1})
		select {
		case msg := <-got:
			if !strings.Contains(msg, "nntp-pool-metrics-updated") {
				t.Fatalf("unexpected message %q", msg)
			}
			return
		case <-deadline:
			t.Fatal("reconnected client received no events")
		case <-time.After(20 * time.Millisecond):
		}
	}
}
