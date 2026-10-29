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
	feedMaxAge = 5 * time.Minute
	// Apple rations background pushes per app and advises two or three an
	// hour, so a desk writing session wakes the phone at most once per
	// spacing (V50 §6.1).
	pushSpacing = 10 * time.Minute
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
	// Called with every file or ack event the desk causes, before the event
	// fans out, so a push held for a listening phone is recorded before the
	// phone's own requests can clear it.
	onDeskChange func(vaultID string)
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
	notify := h.onDeskChange
	h.mu.Unlock()
	if notify != nil && event.From == roleDesk && (event.Kind == "file" || event.Kind == "ack") {
		notify(vaultID)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	for sub := range h.subscribers[vaultID] {
		if event.From != "" && sub.role == event.From {
			continue
		}
		select {
		case sub.events <- event:
		default:
		}
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

// pushCoalescer spaces the silent pushes to each vault's phone. The first
// desk change after a quiet spell is pushed at once; later ones are held and
// go out on a later request once the spacing has passed, carrying the latest
// state. Sends happen inside a request, never on a timer, because Cloud Run
// only gives the instance CPU while it serves one.
type pushCoalescer struct {
	mu       sync.Mutex
	lastSent map[string]time.Time
	held     map[string]bool
	now      func() time.Time
	// send pushes to the vault's phone and reports whether the push must
	// stay held because a phone holds the feed. A stream a killed app left
	// behind looks like a listening phone until the server cuts it.
	send    func(vaultID string) (stillHeld bool)
	spacing time.Duration
}

func newPushCoalescer(send func(vaultID string) bool, now func() time.Time) *pushCoalescer {
	return &pushCoalescer{
		lastSent: map[string]time.Time{},
		held:     map[string]bool{},
		now:      now,
		send:     send,
		spacing:  pushSpacing,
	}
}

// nudge records a desk change and pushes at once if the spacing allows.
func (c *pushCoalescer) nudge(vaultID string) {
	c.mu.Lock()
	c.held[vaultID] = true
	c.mu.Unlock()
	c.flush(vaultID)
}

// flush sends the vault's held push if the spacing has passed. Called after
// every authenticated desk request.
func (c *pushCoalescer) flush(vaultID string) {
	c.mu.Lock()
	previous, sentBefore := c.lastSent[vaultID]
	now := c.now()
	if !c.held[vaultID] || (sentBefore && now.Sub(previous) < c.spacing) {
		c.mu.Unlock()
		return
	}
	// Claim the slot before sending, so a concurrent request doesn't push too.
	delete(c.held, vaultID)
	c.lastSent[vaultID] = now
	c.mu.Unlock()

	if !c.send(vaultID) {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.held[vaultID] = true
	if c.lastSent[vaultID].Equal(now) {
		if sentBefore {
			c.lastSent[vaultID] = previous
		} else {
			delete(c.lastSent, vaultID)
		}
	}
}

// clear drops the vault's held push: the phone is syncing anyway.
func (c *pushCoalescer) clear(vaultID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.held, vaultID)
}

func (c *pushCoalescer) isHeld(vaultID string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.held[vaultID]
}
