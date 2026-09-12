package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// The backend's own record of users, entitlements, and usage: the source of
// truth that a billing driver (Polar, in Stage 2) grants into, never the
// other way around. One JSON file, rewritten atomically on every change;
// the scale this serves (dozens of testers) doesn't justify a database yet,
// and the shape is small enough to migrate when it does.
type state struct {
	Users   map[string]*user   `json:"users"`
	Invites map[string]*invite `json:"invites"`
	Ledger  []ledgerEntry      `json:"ledger"`
}

type userStatus string

const (
	userActive  userStatus = "active"
	userRevoked userStatus = "revoked"
)

type user struct {
	ID             string     `json:"id"`
	Plan           string     `json:"plan"`
	Device         string     `json:"device"`
	InviteCode     string     `json:"invite_code"`
	CredentialHash string     `json:"credential_hash"`
	Status         userStatus `json:"status"`
	CreatedAt      time.Time  `json:"created_at"`
	CycleStartedAt time.Time  `json:"cycle_started_at"`

	Gateway gatewayKey `json:"gateway"`
	// Gateway usage (dollars) when the current cycle started; usage in this
	// cycle is whatever the gateway reports above it.
	UsageBaselineUSD float64 `json:"usage_baseline_usd"`
	// Units burned this cycle, as of the last gateway sync.
	UsedUnits int64 `json:"used_units"`
	// Units granted on top of the plan allowance this cycle (admin top-ups,
	// or taken away when negative). Cleared at rollover.
	AdjustUnits int64     `json:"adjust_units"`
	LastSyncAt  time.Time `json:"last_sync_at"`
	// The gateway key is disabled while true; flipped back when a top-up or
	// a new cycle brings the balance above zero.
	Exhausted bool `json:"exhausted"`
}

type gatewayKey struct {
	Hash string `json:"hash"`
	// The provisioned key itself. Extractable by design (spec decision 12):
	// the budget cap bounds the damage, not secrecy of this file.
	Secret   string  `json:"secret"`
	LimitUSD float64 `json:"limit_usd"`
}

type invite struct {
	Code      string    `json:"code"`
	Plan      string    `json:"plan"`
	MaxUses   int       `json:"max_uses"`
	Uses      int       `json:"uses"`
	Note      string    `json:"note"`
	CreatedAt time.Time `json:"created_at"`
}

type ledgerEntry struct {
	At     time.Time `json:"at"`
	UserID string    `json:"user_id"`
	Units  int64     `json:"units"`
	// "sync" (gateway usage landed), "adjust" (admin), "reset" (admin or
	// cycle rollover), "revoke", "connect".
	Source string `json:"source"`
	Note   string `json:"note,omitempty"`
}

var errNotFound = errors.New("not found")

type store struct {
	path string

	mu    sync.Mutex
	state state
	// Credential hash to user id, rebuilt on load and kept on write.
	byCredential map[string]string
}

func openStore(path string) (*store, error) {
	s := &store{path: path, byCredential: map[string]string{}}
	raw, err := os.ReadFile(path)
	switch {
	case errors.Is(err, os.ErrNotExist):
		s.state = state{Users: map[string]*user{}, Invites: map[string]*invite{}}
		return s, nil
	case err != nil:
		return nil, fmt.Errorf("reading state: %w", err)
	}
	if err := json.Unmarshal(raw, &s.state); err != nil {
		return nil, fmt.Errorf("state file is not valid JSON: %w", err)
	}
	if s.state.Users == nil {
		s.state.Users = map[string]*user{}
	}
	if s.state.Invites == nil {
		s.state.Invites = map[string]*invite{}
	}
	for id, user := range s.state.Users {
		s.byCredential[user.CredentialHash] = id
	}
	return s, nil
}

// update runs fn against the state under the lock and persists the result.
// A failing fn leaves the file untouched; a failing write is reported and
// the in-memory state is rolled back to what the file holds.
func (s *store) update(fn func(state *state) error) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	snapshot, err := json.Marshal(s.state)
	if err != nil {
		return err
	}
	if err := fn(&s.state); err != nil {
		var restored state
		if restoreErr := json.Unmarshal(snapshot, &restored); restoreErr == nil {
			s.state = restored
		}
		return err
	}
	if err := s.persistLocked(); err != nil {
		var restored state
		if restoreErr := json.Unmarshal(snapshot, &restored); restoreErr == nil {
			s.state = restored
		}
		return fmt.Errorf("saving state: %w", err)
	}
	s.byCredential = map[string]string{}
	for id, user := range s.state.Users {
		s.byCredential[user.CredentialHash] = id
	}
	return nil
}

// read runs fn against a copy of the state, so callers can't mutate it
// without going through update.
func (s *store) read(fn func(state state)) {
	s.mu.Lock()
	defer s.mu.Unlock()
	fn(s.state)
}

func (s *store) userByCredential(hash string) (user, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	id, ok := s.byCredential[hash]
	if !ok {
		return user{}, false
	}
	found, ok := s.state.Users[id]
	if !ok {
		return user{}, false
	}
	return *found, true
}

func (s *store) persistLocked() error {
	raw, err := json.MarshalIndent(s.state, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(s.path), 0o700); err != nil {
		return err
	}
	temp := s.path + ".tmp"
	if err := os.WriteFile(temp, raw, 0o600); err != nil {
		return err
	}
	return os.Rename(temp, s.path)
}
