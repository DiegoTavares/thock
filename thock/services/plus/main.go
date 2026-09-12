// The Thock Plus backend (spec: thock/specs/v25-thock-plus-hosted-agent.md,
// Stage 1): users and entitlements in its own store, plans as hot-changeable
// configuration, per-user budget-capped gateway keys, and a usage ledger in
// normalized units. Billing is deliberately absent; a dev invite code is how
// anyone gets a plan for now, and a Polar driver grants into the same store
// later without the app noticing.
//
// Every error body is a plain sentence: the app shows it to the person who
// typed the invite code.
package main

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"math"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"
)

// How long a synced balance is trusted before the gateway is asked again. The
// panel polls the entitlement while a session runs, so this bounds both the
// gateway call rate and how stale a footer can be.
const usageSyncInterval = 30 * time.Second

var logf = log.Printf

type server struct {
	plans      *planCatalog
	store      *store
	gateway    gateway
	adminToken string
	now        func() time.Time
	syncEvery  time.Duration

	// Serializes the sync-and-enforce path per user so two concurrent polls
	// can't both disable (or both re-enable) a key.
	syncMu sync.Mutex
}

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	plansPath := os.Getenv("PLANS_PATH")
	if plansPath == "" {
		plansPath = "plans.json"
	}
	statePath := os.Getenv("STATE_PATH")
	if statePath == "" {
		statePath = "data/state.json"
	}
	adminToken := os.Getenv("ADMIN_TOKEN")
	if adminToken == "" {
		log.Fatal("ADMIN_TOKEN is required; it protects invite minting and allowance edits")
	}

	plans, err := loadPlanCatalog(plansPath)
	if err != nil {
		log.Fatalf("plans: %v", err)
	}
	store, err := openStore(statePath)
	if err != nil {
		log.Fatalf("state: %v", err)
	}
	var gw gateway
	if managementKey := os.Getenv("OPENROUTER_MANAGEMENT_KEY"); managementKey != "" {
		gw = newOpenRouterGateway(managementKey)
	} else {
		log.Print("OPENROUTER_MANAGEMENT_KEY is not set; minting fake keys (nothing will reach a model)")
		gw = newFakeGateway()
	}
	s := newServer(plans, store, gw, adminToken)
	log.Printf("thock plus backend listening on :%s (plans from %s, state in %s)", port, plansPath, statePath)
	log.Fatal(http.ListenAndServe(":"+port, s.routes()))
}

func newServer(plans *planCatalog, store *store, gw gateway, adminToken string) *server {
	return &server{
		plans:      plans,
		store:      store,
		gateway:    gw,
		adminToken: adminToken,
		now:        time.Now,
		syncEvery:  usageSyncInterval,
	}
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/connect", s.handleConnect)
	mux.HandleFunc("GET /v1/entitlement", s.withUser(s.handleEntitlement))
	mux.HandleFunc("POST /v1/disconnect", s.withUser(s.handleDisconnect))

	mux.HandleFunc("GET /admin/plans", s.withAdmin(s.handleAdminPlans))
	mux.HandleFunc("POST /admin/plans/reload", s.withAdmin(s.handleAdminReloadPlans))
	mux.HandleFunc("POST /admin/invites", s.withAdmin(s.handleAdminCreateInvite))
	mux.HandleFunc("GET /admin/invites", s.withAdmin(s.handleAdminListInvites))
	mux.HandleFunc("GET /admin/users", s.withAdmin(s.handleAdminListUsers))
	mux.HandleFunc("POST /admin/users/{id}/allowance", s.withAdmin(s.handleAdminAllowance))
	mux.HandleFunc("POST /admin/users/{id}/revoke", s.withAdmin(s.handleAdminRevoke))
	mux.HandleFunc("GET /admin/users/{id}/ledger", s.withAdmin(s.handleAdminLedger))

	health := func(w http.ResponseWriter, _ *http.Request) { fmt.Fprintln(w, "ok") }
	mux.HandleFunc("GET /health", health)
	mux.HandleFunc("GET /healthz", health)
	mux.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) {
		writeError(w, http.StatusNotFound, "This is the Thock Plus service. There is nothing to browse here.")
	})
	return mux
}

// What the app deserializes (crates/thock/src/plus.rs). Field names are part
// of the app contract; add, don't rename.
type entitlementResponse struct {
	UserID         string    `json:"user_id"`
	Status         string    `json:"status"`
	PlanID         string    `json:"plan_id"`
	PlanName       string    `json:"plan_name"`
	AllowanceUnits int64     `json:"allowance_units"`
	UsedUnits      int64     `json:"used_units"`
	RemainingUnits int64     `json:"remaining_units"`
	WarnAtPercent  int       `json:"warn_at_percent"`
	CycleEndsAt    time.Time `json:"cycle_ends_at"`
	// Present unless revoked. The app puts these into the harness process
	// environment and never shows them.
	Gateway *gatewayGrant `json:"gateway,omitempty"`
	Limits  planLimits    `json:"limits"`
}

type gatewayGrant struct {
	Provider string     `json:"provider"`
	APIKey   string     `json:"api_key"`
	Models   modelTiers `json:"models"`
}

type connectRequest struct {
	InviteCode string `json:"invite_code"`
	Device     string `json:"device"`
}

type connectResponse struct {
	Credential  string              `json:"credential"`
	Entitlement entitlementResponse `json:"entitlement"`
}

func (s *server) handleConnect(w http.ResponseWriter, r *http.Request) {
	var request connectRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "The connect request wasn't readable.")
		return
	}
	code := strings.ToUpper(strings.TrimSpace(request.InviteCode))
	if code == "" {
		writeError(w, http.StatusBadRequest, "Enter an invite code to connect.")
		return
	}

	var invitePlan string
	s.store.read(func(state state) {
		if invite, ok := state.Invites[code]; ok {
			invitePlan = invite.Plan
		}
	})
	if invitePlan == "" {
		writeError(w, http.StatusNotFound, "That invite code isn't one we recognize. Check it and try again.")
		return
	}
	plan, config, ok := s.plans.plan(invitePlan)
	if !ok {
		writeError(w, http.StatusServiceUnavailable, "That invite points at a plan that isn't configured right now. Try again later.")
		return
	}

	userID, err := randomToken(8)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't create your account. Try again.")
		return
	}
	credential, err := randomToken(24)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't create your account. Try again.")
		return
	}
	credential = "tpk_" + credential
	key, err := s.gateway.mint(r.Context(), "thock-plus-"+userID, plan.allowanceDollars(config.UnitsPerDollar))
	if err != nil {
		logf("error: minting a gateway key for a new user: %v", err)
		writeError(w, http.StatusBadGateway, "Couldn't set up your agent's access right now. Try again in a minute.")
		return
	}

	now := s.now()
	created := user{
		ID:             userID,
		Plan:           plan.ID,
		Device:         strings.TrimSpace(request.Device),
		InviteCode:     code,
		CredentialHash: hashCredential(credential),
		Status:         userActive,
		CreatedAt:      now,
		CycleStartedAt: now,
		Gateway:        key,
		LastSyncAt:     now,
	}
	err = s.store.update(func(state *state) error {
		invite, ok := state.Invites[code]
		if !ok {
			return errNotFound
		}
		if invite.MaxUses > 0 && invite.Uses >= invite.MaxUses {
			return errInviteExhausted
		}
		invite.Uses++
		state.Users[userID] = &created
		state.Ledger = append(state.Ledger, ledgerEntry{At: now, UserID: userID, Source: "connect", Note: "invite " + code})
		return nil
	})
	if err != nil {
		if revokeErr := s.gateway.revoke(context.Background(), key.Hash); revokeErr != nil {
			logf("warning: couldn't revoke the key minted for a failed connect: %v", revokeErr)
		}
		switch {
		case errors.Is(err, errInviteExhausted):
			writeError(w, http.StatusGone, "That invite code has been used up.")
		case errors.Is(err, errNotFound):
			writeError(w, http.StatusNotFound, "That invite code isn't one we recognize. Check it and try again.")
		default:
			logf("error: saving a new user: %v", err)
			writeError(w, http.StatusInternalServerError, "Couldn't save your account. Try again.")
		}
		return
	}

	entitlement, err := s.entitlementFor(r.Context(), created)
	if err != nil {
		logf("error: computing a fresh entitlement: %v", err)
		writeError(w, http.StatusInternalServerError, "Your account was created, but its balance couldn't be read. Try again.")
		return
	}
	writeJSON(w, http.StatusOK, connectResponse{Credential: credential, Entitlement: entitlement})
}

var errInviteExhausted = errors.New("invite exhausted")

func (s *server) handleEntitlement(w http.ResponseWriter, r *http.Request, u user) {
	entitlement, err := s.entitlementFor(r.Context(), u)
	if err != nil {
		if errors.Is(err, errPlanMissing) {
			writeError(w, http.StatusServiceUnavailable, "Your plan isn't configured right now. Try again later.")
			return
		}
		logf("error: entitlement for %s: %v", u.ID, err)
		writeError(w, http.StatusBadGateway, "Couldn't read your balance right now. Try again in a minute.")
		return
	}
	writeJSON(w, http.StatusOK, entitlement)
}

func (s *server) handleDisconnect(w http.ResponseWriter, r *http.Request, u user) {
	if err := s.revokeUser(r.Context(), u.ID, "disconnected from the app"); err != nil {
		logf("error: disconnecting %s: %v", u.ID, err)
		writeError(w, http.StatusInternalServerError, "Couldn't disconnect cleanly. Try again.")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

var errPlanMissing = errors.New("plan missing")

// entitlementFor is the allowance loop: roll the cycle over when it has
// ended, pull spend from the gateway when the last sync is stale, compute the
// balance, and enforce it at the gateway (disable at zero, re-enable once a
// top-up or a new cycle restores a balance). Everything the panel footer
// shows comes from here.
func (s *server) entitlementFor(ctx context.Context, u user) (entitlementResponse, error) {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()

	plan, config, ok := s.plans.plan(u.Plan)
	if !ok {
		return entitlementResponse{}, errPlanMissing
	}
	now := s.now()
	if u.Status == userRevoked {
		return entitlementResponse{
			UserID:        u.ID,
			Status:        "revoked",
			PlanID:        plan.ID,
			PlanName:      plan.Name,
			WarnAtPercent: plan.Limits.WarnAtPercent,
			Limits:        plan.Limits,
			CycleEndsAt:   u.CycleStartedAt.Add(plan.cycleLength()),
		}, nil
	}

	changed := false
	cycleEnds := u.CycleStartedAt.Add(plan.cycleLength())
	rolledOver := false
	if !now.Before(cycleEnds) {
		usage, err := s.gateway.usage(ctx, u.Gateway.Hash)
		if err != nil {
			return entitlementResponse{}, err
		}
		// Skip whole cycles that passed while nobody was looking.
		for !now.Before(u.CycleStartedAt.Add(plan.cycleLength())) {
			u.CycleStartedAt = u.CycleStartedAt.Add(plan.cycleLength())
		}
		u.UsageBaselineUSD = usage
		u.UsedUnits = 0
		u.AdjustUnits = 0
		u.LastSyncAt = now
		rolledOver = true
		changed = true
		cycleEnds = u.CycleStartedAt.Add(plan.cycleLength())
	} else if now.Sub(u.LastSyncAt) >= s.syncEvery {
		usage, err := s.gateway.usage(ctx, u.Gateway.Hash)
		if err != nil {
			return entitlementResponse{}, err
		}
		used := int64(math.Round(math.Max(0, usage-u.UsageBaselineUSD) * config.UnitsPerDollar))
		if used != u.UsedUnits {
			u.UsedUnits = used
			changed = true
		}
		u.LastSyncAt = now
	}

	allowance := plan.AllowanceUnits + u.AdjustUnits
	if allowance < 0 {
		allowance = 0
	}
	remaining := allowance - u.UsedUnits
	shouldBeExhausted := remaining <= 0
	// The gateway's own cap is the hard stop even if this service is down:
	// keep it equal to the spend that ends this cycle's allowance.
	wantLimit := u.UsageBaselineUSD + float64(allowance)/config.UnitsPerDollar
	if shouldBeExhausted != u.Exhausted || rolledOver || math.Abs(wantLimit-u.Gateway.LimitUSD) > 1e-9 {
		if err := s.gateway.configure(ctx, u.Gateway.Hash, wantLimit, shouldBeExhausted); err != nil {
			return entitlementResponse{}, err
		}
		u.Exhausted = shouldBeExhausted
		u.Gateway.LimitUSD = wantLimit
		changed = true
	}

	if changed {
		snapshot := u
		err := s.store.update(func(state *state) error {
			stored, ok := state.Users[snapshot.ID]
			if !ok {
				return errNotFound
			}
			if rolledOver {
				state.Ledger = append(state.Ledger, ledgerEntry{At: now, UserID: snapshot.ID, Source: "reset", Note: "new cycle"})
			} else if snapshot.UsedUnits != stored.UsedUnits {
				state.Ledger = append(state.Ledger, ledgerEntry{At: now, UserID: snapshot.ID, Units: snapshot.UsedUnits - stored.UsedUnits, Source: "sync"})
			}
			stored.CycleStartedAt = snapshot.CycleStartedAt
			stored.UsageBaselineUSD = snapshot.UsageBaselineUSD
			stored.UsedUnits = snapshot.UsedUnits
			stored.AdjustUnits = snapshot.AdjustUnits
			stored.LastSyncAt = snapshot.LastSyncAt
			stored.Exhausted = snapshot.Exhausted
			stored.Gateway = snapshot.Gateway
			return nil
		})
		if err != nil {
			return entitlementResponse{}, err
		}
	} else {
		// A sync that changed nothing still moves the clock, and losing that
		// only costs an extra gateway call, so it isn't worth a file write.
		_ = s.store.update(func(state *state) error {
			if stored, ok := state.Users[u.ID]; ok && stored.LastSyncAt.Before(u.LastSyncAt) {
				stored.LastSyncAt = u.LastSyncAt
			}
			return nil
		})
	}

	status := "active"
	if u.Exhausted {
		status = "exhausted"
	}
	return entitlementResponse{
		UserID:         u.ID,
		Status:         status,
		PlanID:         plan.ID,
		PlanName:       plan.Name,
		AllowanceUnits: allowance,
		UsedUnits:      u.UsedUnits,
		RemainingUnits: max(remaining, 0),
		WarnAtPercent:  plan.Limits.WarnAtPercent,
		CycleEndsAt:    cycleEnds,
		Limits:         plan.Limits,
		Gateway: &gatewayGrant{
			Provider: "openrouter",
			APIKey:   u.Gateway.Secret,
			Models:   plan.Models,
		},
	}, nil
}

// revokeUser kills the gateway key first so a revocation holds even if the
// state write fails afterwards.
func (s *server) revokeUser(ctx context.Context, userID, note string) error {
	var hash string
	var found bool
	s.store.read(func(state state) {
		if u, ok := state.Users[userID]; ok {
			hash = u.Gateway.Hash
			found = ok && u.Status != userRevoked
		}
	})
	if !found {
		return errNotFound
	}
	if err := s.gateway.revoke(ctx, hash); err != nil {
		return err
	}
	return s.store.update(func(state *state) error {
		u, ok := state.Users[userID]
		if !ok {
			return errNotFound
		}
		u.Status = userRevoked
		u.Gateway.Secret = ""
		u.Exhausted = true
		state.Ledger = append(state.Ledger, ledgerEntry{At: s.now(), UserID: userID, Source: "revoke", Note: note})
		return nil
	})
}

// --- admin ---

func (s *server) handleAdminPlans(w http.ResponseWriter, _ *http.Request) {
	config := s.plans.current()
	writeJSON(w, http.StatusOK, map[string]any{
		"version":          config.Version,
		"units_per_dollar": config.UnitsPerDollar,
		"plans":            config.sortedPlans(),
	})
}

func (s *server) handleAdminReloadPlans(w http.ResponseWriter, _ *http.Request) {
	if err := s.plans.reload(); err != nil {
		writeError(w, http.StatusUnprocessableEntity, "The plans file didn't load, the previous plans stay in effect: "+err.Error())
		return
	}
	s.handleAdminPlans(w, nil)
}

type createInviteRequest struct {
	Plan    string `json:"plan"`
	MaxUses int    `json:"max_uses"`
	Note    string `json:"note"`
}

func (s *server) handleAdminCreateInvite(w http.ResponseWriter, r *http.Request) {
	var request createInviteRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "The invite request wasn't readable.")
		return
	}
	if _, _, ok := s.plans.plan(request.Plan); !ok {
		writeError(w, http.StatusUnprocessableEntity, fmt.Sprintf("There is no plan %q in the plans file.", request.Plan))
		return
	}
	code, err := inviteCode()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't mint an invite code.")
		return
	}
	created := invite{Code: code, Plan: request.Plan, MaxUses: request.MaxUses, Note: request.Note, CreatedAt: s.now()}
	err = s.store.update(func(state *state) error {
		state.Invites[code] = &created
		return nil
	})
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't save the invite: "+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, created)
}

func (s *server) handleAdminListInvites(w http.ResponseWriter, _ *http.Request) {
	var invites []invite
	s.store.read(func(state state) {
		for _, invite := range state.Invites {
			invites = append(invites, *invite)
		}
	})
	writeJSON(w, http.StatusOK, invites)
}

type adminUser struct {
	ID             string     `json:"id"`
	Plan           string     `json:"plan"`
	Device         string     `json:"device"`
	InviteCode     string     `json:"invite_code"`
	Status         userStatus `json:"status"`
	CreatedAt      time.Time  `json:"created_at"`
	CycleStartedAt time.Time  `json:"cycle_started_at"`
	UsedUnits      int64      `json:"used_units"`
	AdjustUnits    int64      `json:"adjust_units"`
	Exhausted      bool       `json:"exhausted"`
	GatewayKeyHash string     `json:"gateway_key_hash"`
}

func (s *server) handleAdminListUsers(w http.ResponseWriter, _ *http.Request) {
	var users []adminUser
	s.store.read(func(state state) {
		for _, u := range state.Users {
			users = append(users, adminUser{
				ID: u.ID, Plan: u.Plan, Device: u.Device, InviteCode: u.InviteCode, Status: u.Status,
				CreatedAt: u.CreatedAt, CycleStartedAt: u.CycleStartedAt, UsedUnits: u.UsedUnits,
				AdjustUnits: u.AdjustUnits, Exhausted: u.Exhausted, GatewayKeyHash: u.Gateway.Hash,
			})
		}
	})
	writeJSON(w, http.StatusOK, users)
}

type allowanceRequest struct {
	// Start a fresh cycle now: usage back to zero, adjustments cleared.
	Reset bool `json:"reset"`
	// Units added to (or, negative, taken from) this cycle's allowance.
	AdjustUnits int64 `json:"adjust_units"`
	// Move the user to another plan from the next request on.
	Plan string `json:"plan"`
	Note string `json:"note"`
}

func (s *server) handleAdminAllowance(w http.ResponseWriter, r *http.Request) {
	var request allowanceRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "The allowance request wasn't readable.")
		return
	}
	userID := r.PathValue("id")
	if request.Plan != "" {
		if _, _, ok := s.plans.plan(request.Plan); !ok {
			writeError(w, http.StatusUnprocessableEntity, fmt.Sprintf("There is no plan %q in the plans file.", request.Plan))
			return
		}
	}
	var hash string
	s.store.read(func(state state) {
		if u, ok := state.Users[userID]; ok {
			hash = u.Gateway.Hash
		}
	})
	if hash == "" {
		writeError(w, http.StatusNotFound, "No such user.")
		return
	}
	var baseline float64
	if request.Reset {
		usage, err := s.gateway.usage(r.Context(), hash)
		if err != nil {
			writeError(w, http.StatusBadGateway, "Couldn't read the gateway usage to reset from: "+err.Error())
			return
		}
		baseline = usage
	}
	now := s.now()
	err := s.store.update(func(state *state) error {
		u, ok := state.Users[userID]
		if !ok {
			return errNotFound
		}
		if request.Plan != "" {
			u.Plan = request.Plan
		}
		if request.Reset {
			u.CycleStartedAt = now
			u.UsageBaselineUSD = baseline
			u.UsedUnits = 0
			u.AdjustUnits = 0
			state.Ledger = append(state.Ledger, ledgerEntry{At: now, UserID: userID, Source: "reset", Note: request.Note})
		}
		if request.AdjustUnits != 0 {
			u.AdjustUnits += request.AdjustUnits
			state.Ledger = append(state.Ledger, ledgerEntry{At: now, UserID: userID, Units: -request.AdjustUnits, Source: "adjust", Note: request.Note})
		}
		// Force the next entitlement read to re-sync and re-enforce.
		u.LastSyncAt = time.Time{}
		return nil
	})
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't update the allowance: "+err.Error())
		return
	}
	var updated user
	s.store.read(func(state state) {
		if u, ok := state.Users[userID]; ok {
			updated = *u
		}
	})
	entitlement, err := s.entitlementFor(r.Context(), updated)
	if err != nil {
		writeError(w, http.StatusBadGateway, "The allowance was saved but couldn't be applied at the gateway: "+err.Error())
		return
	}
	entitlement.Gateway = nil
	writeJSON(w, http.StatusOK, entitlement)
}

func (s *server) handleAdminRevoke(w http.ResponseWriter, r *http.Request) {
	err := s.revokeUser(r.Context(), r.PathValue("id"), "revoked by admin")
	switch {
	case errors.Is(err, errNotFound):
		writeError(w, http.StatusNotFound, "No such active user.")
	case err != nil:
		writeError(w, http.StatusBadGateway, "Couldn't revoke: "+err.Error())
	default:
		w.WriteHeader(http.StatusNoContent)
	}
}

func (s *server) handleAdminLedger(w http.ResponseWriter, r *http.Request) {
	userID := r.PathValue("id")
	entries := []ledgerEntry{}
	s.store.read(func(state state) {
		for _, entry := range state.Ledger {
			if entry.UserID == userID {
				entries = append(entries, entry)
			}
		}
	})
	writeJSON(w, http.StatusOK, entries)
}

// --- plumbing ---

func (s *server) withUser(next func(http.ResponseWriter, *http.Request, user)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		credential, ok := bearer(r)
		if !ok {
			writeError(w, http.StatusUnauthorized, "This request needs your Thock Plus credential.")
			return
		}
		u, ok := s.store.userByCredential(hashCredential(credential))
		if !ok {
			writeError(w, http.StatusUnauthorized, "That Thock Plus connection is no longer valid. Connect again with an invite code.")
			return
		}
		if u.Status == userRevoked {
			writeError(w, http.StatusForbidden, "Your Thock Plus access was turned off. Your own agent still works from the Agent panel.")
			return
		}
		next(w, r, u)
	}
}

func (s *server) withAdmin(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		token, ok := bearer(r)
		if !ok || token != s.adminToken {
			writeError(w, http.StatusUnauthorized, "Admin token required.")
			return
		}
		next(w, r)
	}
}

func bearer(r *http.Request) (string, bool) {
	header := r.Header.Get("Authorization")
	const prefix = "Bearer "
	if !strings.HasPrefix(header, prefix) {
		return "", false
	}
	token := strings.TrimSpace(strings.TrimPrefix(header, prefix))
	return token, token != ""
}

func hashCredential(credential string) string {
	sum := sha256.Sum256([]byte(credential))
	return hex.EncodeToString(sum[:])
}

func randomToken(bytes int) (string, error) {
	buffer := make([]byte, bytes)
	if _, err := rand.Read(buffer); err != nil {
		return "", err
	}
	return hex.EncodeToString(buffer), nil
}

// Invite codes are typed by hand, so they avoid look-alike characters and
// read as three groups: THOCK-7K3M-P9QX.
func inviteCode() (string, error) {
	const alphabet = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"
	buffer := make([]byte, 8)
	if _, err := rand.Read(buffer); err != nil {
		return "", err
	}
	var code strings.Builder
	code.WriteString("THOCK-")
	for i, b := range buffer {
		if i == 4 {
			code.WriteByte('-')
		}
		code.WriteByte(alphabet[int(b)%len(alphabet)])
	}
	return code.String(), nil
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(body); err != nil {
		logf("error: writing response: %v", err)
	}
}

func writeError(w http.ResponseWriter, status int, message string) {
	writeJSON(w, status, map[string]string{"error": message})
}
