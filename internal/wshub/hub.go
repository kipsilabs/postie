// Package wshub fans backend events out to browser clients over WebSocket in
// web mode.
package wshub

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

const (
	defaultPingInterval   = 30 * time.Second
	defaultPongWait       = 60 * time.Second
	defaultWriteWait      = 10 * time.Second
	defaultSendBufferSize = 256
)

var upgrader = websocket.Upgrader{
	ReadBufferSize:  1024,
	WriteBufferSize: 1024,
	CheckOrigin: func(r *http.Request) bool {
		return true
	},
}

type client struct {
	conn *websocket.Conn
	send chan []byte
	hub  *Hub
	id   string
}

// Hub manages WebSocket connections and broadcasts events to all of them.
type Hub struct {
	clients    map[*client]bool
	broadcast  chan []byte
	register   chan *client
	unregister chan *client
	mu         sync.RWMutex

	pingInterval time.Duration
	pongWait     time.Duration
	writeWait    time.Duration
}

type message struct {
	Type string `json:"type"`
	Data any    `json:"data"`
}

// NewHub creates a hub. Call Run in its own goroutine before serving clients.
func NewHub() *Hub {
	return &Hub{
		clients:      make(map[*client]bool),
		broadcast:    make(chan []byte, 256),
		register:     make(chan *client),
		unregister:   make(chan *client),
		pingInterval: defaultPingInterval,
		pongWait:     defaultPongWait,
		writeWait:    defaultWriteWait,
	}
}

// Run drives registration and broadcast. It never returns.
func (h *Hub) Run() {
	for {
		select {
		case c := <-h.register:
			h.mu.Lock()
			h.clients[c] = true
			h.mu.Unlock()
			slog.Info("WebSocket client connected", "id", c.id)

		case c := <-h.unregister:
			h.mu.Lock()
			if _, ok := h.clients[c]; ok {
				delete(h.clients, c)
				close(c.send)
				slog.Info("WebSocket client disconnected", "id", c.id)
			}
			h.mu.Unlock()

		case msg := <-h.broadcast:
			h.mu.Lock()
			for c := range h.clients {
				select {
				case c.send <- msg:
				default:
					// A peer that stopped draining is dead or hopelessly behind;
					// drop it so one stalled tab cannot hold the hub.
					delete(h.clients, c)
					close(c.send)
					slog.Warn("WebSocket client too slow, dropping", "id", c.id)
				}
			}
			h.mu.Unlock()
		}
	}
}

// EmitEvent sends an event to all connected clients.
func (h *Hub) EmitEvent(eventType string, data any) {
	payload, err := json.Marshal(message{Type: eventType, Data: data})
	if err != nil {
		slog.Error("Error marshaling WebSocket message", "error", err)
		return
	}

	select {
	case h.broadcast <- payload:
	default:
		slog.Warn("WebSocket broadcast channel full, dropping message")
	}
}

// ServeHTTP upgrades the request and attaches the connection to the hub.
func (h *Hub) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		slog.Error("WebSocket upgrade error", "error", err)
		return
	}

	c := &client{
		conn: conn,
		send: make(chan []byte, defaultSendBufferSize),
		hub:  h,
		id:   fmt.Sprintf("client-%s", conn.RemoteAddr().String()),
	}
	h.register <- c

	go c.writePump()
	go c.readPump()
}

func (c *client) readPump() {
	defer func() {
		c.hub.unregister <- c
		_ = c.conn.Close()
	}()

	// Without a read deadline a half-open TCP connection (NAT timeout, proxy
	// idle cut) never errors, and the browser side never learns to reconnect.
	// Pongs push the deadline forward; silence past pongWait ends the client.
	_ = c.conn.SetReadDeadline(time.Now().Add(c.hub.pongWait))
	c.conn.SetPongHandler(func(string) error {
		return c.conn.SetReadDeadline(time.Now().Add(c.hub.pongWait))
	})

	for {
		if _, _, err := c.conn.ReadMessage(); err != nil {
			if websocket.IsUnexpectedCloseError(err, websocket.CloseGoingAway, websocket.CloseAbnormalClosure) {
				slog.Debug("WebSocket read ended", "id", c.id, "error", err)
			}
			return
		}
	}
}

func (c *client) writePump() {
	ticker := time.NewTicker(c.hub.pingInterval)
	defer func() {
		ticker.Stop()
		_ = c.conn.Close()
	}()

	for {
		select {
		case msg, ok := <-c.send:
			if !ok {
				return
			}
			if err := c.write(websocket.TextMessage, msg); err != nil {
				slog.Debug("WebSocket write failed", "id", c.id, "error", err)
				return
			}
		case <-ticker.C:
			if err := c.write(websocket.PingMessage, nil); err != nil {
				slog.Debug("WebSocket ping failed", "id", c.id, "error", err)
				return
			}
		}
	}
}

func (c *client) write(messageType int, payload []byte) error {
	if err := c.conn.SetWriteDeadline(time.Now().Add(c.hub.writeWait)); err != nil {
		return err
	}
	return c.conn.WriteMessage(messageType, payload)
}
