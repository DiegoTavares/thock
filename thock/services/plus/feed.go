package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"
)

// The change feed (contract §6.5): one server-sent-events stream per
// connected phone, carrying nudges caused by the desk and vault-level
// changes. The desk polls rather than hold a stream; the `write` event a
// phone causes is still published, for desks older than that. Events are
// hints, never the record; clients poll on every (re)connect regardless.

const (
	feedPingInterval = 25 * time.Second
	// An open stream keeps the Cloud Run instance billed, and a phone iOS
	// suspends mid-stream never closes it: the kernel still acknowledges the
	// pings. Ending every stream after a few minutes bounds what one costs;
	// a phone still in the foreground reconnects.
	feedMaxAge      = 5 * time.Minute
	pushBurstWindow = 30 * time.Second
)

type feedEvent struct {
	Kind string
	Data any
	// Which role caused it; subscribers with this role don't receive it.
	// Empty means everyone (vault-level events).
	From deviceRole
}

type feedSubscriber struct {
	role   deviceRole
	events chan feedEvent
}

type feedHub struct {
	mu          sync.Mutex
	subscribers map[string]map[*feedSubscriber]struct{}
	// Called when a file or ack event finds no phone listening.
	onPhoneAbsent func(vaultID string)
	// How long one stream lives; feedMaxAge outside tests.
	maxAge time.Duration
}

func newFeedHub() *feedHub {
	return &feedHub{subscribers: map[string]map[*feedSubscriber]struct{}{}, maxAge: feedMaxAge}
}

func (h *feedHub) subscribe(vaultID string, role deviceRole) (*feedSubscriber, func()) {
	sub := &feedSubscriber{role: role, events: make(chan feedEvent, 64)}
	h.mu.Lock()
	if h.subscribers[vaultID] == nil {
		h.subscribers[vaultID] = map[*feedSubscriber]struct{}{}
	}
	h.subscribers[vaultID][sub] = struct{}{}
	h.mu.Unlock()
	return sub, func() {
		h.mu.Lock()
		defer h.mu.Unlock()
		if subs := h.subscribers[vaultID]; subs != nil {
			delete(subs, sub)
			if len(subs) == 0 {
				delete(h.subscribers, vaultID)
			}
		}
	}
}

func (h *feedHub) phoneListening(vaultID string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	for sub := range h.subscribers[vaultID] {
		if sub.role == rolePhone {
			return true
		}
	}
	return false
}

// publish fans the event out to every subscriber of the vault except those
// with the originating role. A subscriber that isn't draining its channel
// loses the event; it will catch up by polling on its next reconnect.
func (h *feedHub) publish(vaultID string, event feedEvent) {
	h.mu.Lock()
	phoneHeard := false
	for sub := range h.subscribers[vaultID] {
		if event.From != "" && sub.role == event.From {
			continue
		}
		if sub.role == rolePhone {
			phoneHeard = true
		}
		select {
		case sub.events <- event:
		default:
		}
	}
	notify := h.onPhoneAbsent
	h.mu.Unlock()
	if !phoneHeard && notify != nil && event.From == roleDesk && (event.Kind == "file" || event.Kind == "ack") {
		notify(vaultID)
	}
}

// serve writes the stream until the client goes away or maxAge passes;
// clients reconnect. Cloud Run's own cut, --timeout in deploy.sh and
// deploy-services.yml, sits well above maxAge.
// stillAllowed is asked at every ping, so a credential revoked, replaced or
// reset stops receiving events within one interval instead of at the end of
// the stream.
func (h *feedHub) serve(w http.ResponseWriter, r *http.Request, vaultID string, role deviceRole, stillAllowed func(context.Context) bool) {
	controller := http.NewResponseController(w)
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)
	if _, err := fmt.Fprint(w, ": connected\n\n"); err != nil {
		return
	}
	if err := controller.Flush(); err != nil {
		return
	}

	sub, unsubscribe := h.subscribe(vaultID, role)
	defer unsubscribe()
	ping := time.NewTicker(feedPingInterval)
	defer ping.Stop()
	expired := time.NewTimer(h.maxAge)
	defer expired.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case <-expired.C:
			return
		case <-ping.C:
			if stillAllowed != nil && !stillAllowed(r.Context()) {
				return
			}
			if _, err := fmt.Fprint(w, ": ping\n\n"); err != nil {
				return
			}
			if err := controller.Flush(); err != nil {
				return
			}
		case event := <-sub.events:
			data, err := json.Marshal(event.Data)
			if err != nil {
				logf("error: encoding a feed event: %v", err)
				continue
			}
			if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event.Kind, data); err != nil {
				return
			}
			if err := controller.Flush(); err != nil {
				return
			}
		}
	}
}

// --- push ---

// pusher delivers a silent push to the phone: apnsPusher (apns.go) when an
// APNs key is configured, the logging fake otherwise. An error wrapping
// errPushTokenGone tells the caller to forget the token.
type pusher interface {
	push(ctx context.Context, apnsToken string, payload []byte) error
}

type loggingPusher struct {
	mu   sync.Mutex
	sent []pushRecord
}

type pushRecord struct {
	Token   string
	Payload []byte
}

func (p *loggingPusher) push(_ context.Context, token string, payload []byte) error {
	p.mu.Lock()
	p.sent = append(p.sent, pushRecord{Token: token, Payload: payload})
	p.mu.Unlock()
	logf("push (not sent, no APNs configured) to %s…: %s", token[:min(6, len(token))], payload)
	return nil
}

func (p *loggingPusher) records() []pushRecord {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]pushRecord(nil), p.sent...)
}

// pushCoalescer sends at most one push per vault per burst window: the
// first change goes out at once, later ones in the same window collapse
// into a single push at the window's end.
type pushCoalescer struct {
	mu       sync.Mutex
	lastSent map[string]time.Time
	pending  map[string]*time.Timer
	now      func() time.Time
	send     func(vaultID string)
	window   time.Duration
}

func newPushCoalescer(send func(vaultID string), now func() time.Time) *pushCoalescer {
	return &pushCoalescer{
		lastSent: map[string]time.Time{},
		pending:  map[string]*time.Timer{},
		now:      now,
		send:     send,
		window:   pushBurstWindow,
	}
}

func (c *pushCoalescer) nudge(vaultID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	now := c.now()
	wait := c.window - now.Sub(c.lastSent[vaultID])
	if timer := c.pending[vaultID]; timer != nil {
		if wait > 0 {
			return
		}
		// The window closed while a timer was still waiting on it (the
		// clock moved faster than the timer): send now instead.
		timer.Stop()
		delete(c.pending, vaultID)
	}
	if wait <= 0 {
		c.lastSent[vaultID] = now
		go c.send(vaultID)
		return
	}
	c.pending[vaultID] = time.AfterFunc(wait, func() {
		c.mu.Lock()
		delete(c.pending, vaultID)
		c.lastSent[vaultID] = c.now()
		c.mu.Unlock()
		c.send(vaultID)
	})
}

// stop cancels every pending timer; for tests and shutdown.
func (c *pushCoalescer) stop() {
	c.mu.Lock()
	defer c.mu.Unlock()
	for id, timer := range c.pending {
		timer.Stop()
		delete(c.pending, id)
	}
}
