// The Thock Plus backend (spec: thock/specs/v25-thock-plus-hosted-agent.md,
// Stage 1): users and entitlements in Postgres, plans as hot-changeable rows,
// per-user budget-capped gateway keys, and a usage ledger in normalized
// units. Billing is deliberately absent; a dev invite code is how anyone gets
// a plan for now, and a Polar driver grants into the same store later without
// the app noticing.
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
	"path/filepath"
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
	store      *store
	gateway    gateway
	adminToken string
	now        func() time.Time
	syncEvery  time.Duration
	// How long one gateway call may take while a phone key is being dropped.
	gatewayPatience time.Duration

	// Vault sync (thock/specs/v34-vault-sync-api.md).
	blobs     blobStore
	feed      *feedHub
	pusher    pusher
	coalescer *pushCoalescer
	// Where clients reach this process; the local blob store signs URLs
	// under it.
	publicURL string

	// Serializes the sync-and-enforce path so two concurrent polls can't
	// both disable (or both re-enable) a key, and so the phone key is minted
	// and revoked between passes, never during one. Per process, which is why
	// the deployment runs one instance.
	syncMu sync.Mutex
}

func main() {
	databaseURL := os.Getenv("DATABASE_URL")
	if databaseURL == "" {
		log.Fatal("DATABASE_URL is required (a postgres:// URL)")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	if len(os.Args) > 1 && os.Args[1] == "migrate" {
		pool, err := openPool(ctx, databaseURL)
		if err != nil {
			log.Fatalf("database: %v", err)
		}
		defer pool.Close()
		applied, err := migrate(ctx, pool)
		for _, name := range applied {
			log.Printf("applied %s", name)
		}
		if err != nil {
			log.Fatalf("migrate: %v", err)
		}
		log.Printf("schema is up to date (%d applied now)", len(applied))
		return
	}

	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	adminToken := os.Getenv("ADMIN_TOKEN")
	if adminToken == "" {
		log.Fatal("ADMIN_TOKEN is required; it protects invite minting and allowance edits")
	}

	store, err := openStore(ctx, databaseURL)
	if err != nil {
		log.Fatalf("database: %v", err)
	}
	defer store.close()
	var gw gateway
	if managementKey := os.Getenv("OPENROUTER_MANAGEMENT_KEY"); managementKey != "" {
		gw = newOpenRouterGateway(managementKey)
	} else {
		log.Print("OPENROUTER_MANAGEMENT_KEY is not set; minting fake keys (nothing will reach a model)")
		gw = newFakeGateway()
	}
	s := newServer(store, gw, adminToken)
	if publicURL := os.Getenv("PUBLIC_URL"); publicURL != "" {
		s.publicURL = strings.TrimRight(publicURL, "/")
	} else {
		s.publicURL = "http://localhost:" + port
	}
	switch kind := os.Getenv("BLOB_STORE"); kind {
	case "", "local":
		dir := os.Getenv("BLOB_DIR")
		if dir == "" {
			dir = filepath.Join(os.TempDir(), "thock-plus-blobs")
		}
		key := []byte(os.Getenv("BLOB_SIGNING_KEY"))
		if len(key) == 0 {
			// A per-process key: signed URLs die with the process, which is
			// fine for a store that lives on the same machine.
			key = make([]byte, 32)
			if _, err := rand.Read(key); err != nil {
				log.Fatalf("blob signing key: %v", err)
			}
		}
		blobs, err := newLocalBlobStore(dir, key, func() string { return s.publicURL })
		if err != nil {
			log.Fatalf("blob store: %v", err)
		}
		s.blobs = blobs
		log.Printf("storing blobs under %s", dir)
	case "gcs":
		bucket := os.Getenv("BLOB_BUCKET")
		blobs, err := newGCSBlobStore(context.Background(), bucket)
		if err != nil {
			log.Fatalf("blob store: %v", err)
		}
		s.blobs = blobs
		log.Printf("storing blobs in the gs://%s bucket", bucket)
	default:
		log.Fatalf("BLOB_STORE=%q isn't supported; use local or gcs", kind)
	}
	go s.runSweeper(context.Background(), time.Hour)
	log.Printf("thock plus backend listening on :%s", port)
	log.Fatal(http.ListenAndServe(":"+port, s.routes()))
}

func newServer(store *store, gw gateway, adminToken string) *server {
	s := &server{
		store:           store,
		gateway:         gw,
		adminToken:      adminToken,
		now:             time.Now,
		syncEvery:       usageSyncInterval,
		gatewayPatience: 8 * time.Second,
		feed:            newFeedHub(),
		pusher:          &loggingPusher{},
		publicURL:       "http://localhost:8080",
	}
	s.coalescer = newPushCoalescer(s.sendPush, func() time.Time { return s.now() })
	s.feed.onPhoneAbsent = s.coalescer.nudge
	return s
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/connect", s.handleConnect)
	mux.HandleFunc("GET /v1/entitlement", s.withUser(s.handleEntitlement))
	mux.HandleFunc("POST /v1/disconnect", s.withUser(s.handleDisconnect))

	mux.HandleFunc("POST /v1/vault", s.withVault(vaultCreateRoute, s.handleVaultCreate))
	mux.HandleFunc("GET /v1/vault", s.withVault(vaultStatusRoute, s.handleVaultGet))
	mux.HandleFunc("DELETE /v1/vault", s.withVault(vaultDeleteRoute, s.handleVaultDelete))
	mux.HandleFunc("POST /v1/vault/reset", s.withVault(deskOnly, s.handleVaultReset))
	mux.HandleFunc("POST /v1/vault/pairings", s.withVault(deskOnly, s.handlePairingCreate))
	mux.HandleFunc("POST /v1/vault/pair", s.handlePair)
	mux.HandleFunc("GET /v1/vault/devices", s.withVault(bothRead, s.handleDevicesList))
	mux.HandleFunc("PATCH /v1/vault/devices/me", s.withVault(phoneOnly, s.handleDevicePatch))
	mux.HandleFunc("POST /v1/vault/devices/{device_id}/revoke", s.withVault(deskOnly, s.handleDeviceRevoke))
	mux.HandleFunc("GET /v1/vault/files", s.withVault(bothRead, s.handleFilesList))
	mux.HandleFunc("POST /v1/vault/files/{path...}", s.withVault(deskOnly, s.handleFilePost))
	mux.HandleFunc("DELETE /v1/vault/files/{path...}", s.withVault(deskOnly, s.handleFileDelete))
	mux.HandleFunc("POST /v1/vault/writes", s.withVault(phoneOnly, s.handleWriteCreate))
	mux.HandleFunc("GET /v1/vault/writes", s.withVault(deskOnly, s.handleWritesList))
	mux.HandleFunc("POST /v1/vault/writes/ack", s.withVault(deskOnly, s.handleWritesAck))
	mux.HandleFunc("GET /v1/vault/feed", s.withVault(bothRead, s.handleFeed))
	mux.HandleFunc("GET /v1/vault/agent", s.withVault(phoneOnly, s.handleAgentGrant))
	if local, ok := s.blobs.(*localBlobStore); ok {
		mux.HandleFunc("PUT /v1/vault/blobs/{token}", local.serve)
		mux.HandleFunc("GET /v1/vault/blobs/{token}", local.serve)
	}

	mux.HandleFunc("GET /admin/plans", s.withAdmin(s.handleAdminPlans))
	mux.HandleFunc("PUT /admin/plans/{id}", s.withAdmin(s.handleAdminPutPlan))
	mux.HandleFunc("PUT /admin/settings", s.withAdmin(s.handleAdminPutSettings))
	mux.HandleFunc("POST /admin/invites", s.withAdmin(s.handleAdminCreateInvite))
	mux.HandleFunc("GET /admin/invites", s.withAdmin(s.handleAdminListInvites))
	mux.HandleFunc("GET /admin/users", s.withAdmin(s.handleAdminListUsers))
	mux.HandleFunc("POST /admin/users/{id}/allowance", s.withAdmin(s.handleAdminAllowance))
	mux.HandleFunc("POST /admin/users/{id}/revoke", s.withAdmin(s.handleAdminRevoke))
	mux.HandleFunc("GET /admin/users/{id}/ledger", s.withAdmin(s.handleAdminLedger))
	mux.HandleFunc("POST /admin/users/{id}/vault/lapse", s.withAdmin(s.handleAdminLapse))

	health := func(w http.ResponseWriter, r *http.Request) {
		if err := s.store.pool.Ping(r.Context()); err != nil {
			writeError(w, http.StatusServiceUnavailable, "The database isn't reachable.")
			return
		}
		fmt.Fprintln(w, "ok")
	}
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
	// Present when the user has a vault (contract §6.6).
	Vault *entitlementVault `json:"vault,omitempty"`
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

	found, err := s.store.inviteByCode(r.Context(), code)
	if errors.Is(err, errNotFound) {
		writeError(w, http.StatusNotFound, "That invite code isn't one we recognize. Check it and try again.")
		return
	}
	if err != nil {
		logf("error: looking up invite %s: %v", code, err)
		writeError(w, http.StatusInternalServerError, "Couldn't check that invite right now. Try again.")
		return
	}
	if found.MaxUses > 0 && found.Uses >= found.MaxUses {
		writeError(w, http.StatusGone, "That invite code has been used up.")
		return
	}
	plan, config, err := s.store.plan(r.Context(), found.Plan)
	if err != nil {
		logf("error: plan %s for invite %s: %v", found.Plan, code, err)
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
	err = s.store.createUser(r.Context(), created, ledgerEntry{At: now, UserID: userID, Source: "connect", Note: "invite " + code})
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

	entitlement, err := s.entitlementFor(r.Context(), created.ID)
	if err != nil {
		logf("error: computing a fresh entitlement: %v", err)
		writeError(w, http.StatusInternalServerError, "Your account was created, but its balance couldn't be read. Try again.")
		return
	}
	writeJSON(w, http.StatusOK, connectResponse{Credential: credential, Entitlement: entitlement})
}

func (s *server) handleEntitlement(w http.ResponseWriter, r *http.Request, u user) {
	entitlement, err := s.entitlementFor(r.Context(), u.ID)
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

// entitlementFor runs the allowance loop for the desk. The user is read under
// the lock: a row fetched before it could miss a phone key minted since, and
// the pass would then enforce the balance on one key only.
func (s *server) entitlementFor(ctx context.Context, userID string) (entitlementResponse, error) {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()
	u, err := s.store.userByID(ctx, userID)
	if err != nil {
		return entitlementResponse{}, err
	}
	return s.allowanceLoop(ctx, u)
}

// keyUsage reads cumulative spend for the desk key and, when there is one,
// the phone key.
func (s *server) keyUsage(ctx context.Context, u user) (desk, phone float64, err error) {
	desk, err = s.gateway.usage(ctx, u.Gateway.Hash)
	if err != nil {
		return 0, 0, err
	}
	if u.PhoneGateway.Hash != "" {
		phone, err = s.gateway.usage(ctx, u.PhoneGateway.Hash)
		if err != nil {
			return 0, 0, err
		}
	}
	return desk, phone, nil
}

// allowanceLoop rolls the cycle over when it has ended, pulls spend from the
// gateway when the last sync is stale, computes the balance, and enforces it
// at the gateway (disable at zero, re-enable once a top-up or a new cycle
// restores a balance). The allowance is one pool: spend on the desk key and
// on the phone key, each above its own cycle baseline, counts against it, and
// both keys are disabled and re-enabled together. Everything the panel footer
// and the phone's grant show comes from here. Callers hold syncMu.
func (s *server) allowanceLoop(ctx context.Context, u user) (entitlementResponse, error) {
	plan, config, err := s.store.plan(ctx, u.Plan)
	if errors.Is(err, errNotFound) {
		return entitlementResponse{}, errPlanMissing
	}
	if err != nil {
		return entitlementResponse{}, err
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

	before := u
	changed := false
	cycleEnds := u.CycleStartedAt.Add(plan.cycleLength())
	rolledOver := false
	if !now.Before(cycleEnds) {
		usage, phoneUsage, err := s.keyUsage(ctx, u)
		if err != nil {
			return entitlementResponse{}, err
		}
		// Skip whole cycles that passed while nobody was looking.
		for !now.Before(u.CycleStartedAt.Add(plan.cycleLength())) {
			u.CycleStartedAt = u.CycleStartedAt.Add(plan.cycleLength())
		}
		u.UsageBaselineUSD = usage
		u.PhoneUsageBaselineUSD = phoneUsage
		u.UsedUnits = 0
		u.AdjustUnits = 0
		u.LastSyncAt = now
		rolledOver = true
		changed = true
		cycleEnds = u.CycleStartedAt.Add(plan.cycleLength())
	} else if now.Sub(u.LastSyncAt) >= s.syncEvery {
		usage, phoneUsage, err := s.keyUsage(ctx, u)
		if err != nil {
			return entitlementResponse{}, err
		}
		spent := math.Max(0, usage-u.UsageBaselineUSD) + math.Max(0, phoneUsage-u.PhoneUsageBaselineUSD)
		used := int64(math.Round(spent * config.UnitsPerDollar))
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
	// keep each key's equal to the spend that would end this cycle's
	// allowance on that key alone. Unreachable and with both keys abused,
	// that is twice the allowance at worst (V35 §5.1).
	// A baseline lowered by a departed phone key's spend can put the desk
	// key's cap below zero, which no gateway should be asked to store.
	wantLimit := math.Max(0, u.UsageBaselineUSD+float64(allowance)/config.UnitsPerDollar)
	wantPhoneLimit := u.PhoneUsageBaselineUSD + float64(allowance)/config.UnitsPerDollar
	flipped := shouldBeExhausted != u.Exhausted || rolledOver
	configureDesk := flipped || math.Abs(wantLimit-u.Gateway.LimitUSD) > 1e-9
	configurePhone := u.PhoneGateway.Hash != "" && (flipped || math.Abs(wantPhoneLimit-u.PhoneGateway.LimitUSD) > 1e-9)
	if configureDesk {
		if err := s.gateway.configure(ctx, u.Gateway.Hash, wantLimit, shouldBeExhausted); err != nil {
			return entitlementResponse{}, err
		}
		u.Gateway.LimitUSD = wantLimit
	}
	if configurePhone {
		// A failure here leaves the desk key already switched and nothing
		// saved, so the next pass still sees the flip and configures both.
		if err := s.gateway.configure(ctx, u.PhoneGateway.Hash, wantPhoneLimit, shouldBeExhausted); err != nil {
			return entitlementResponse{}, err
		}
		u.PhoneGateway.LimitUSD = wantPhoneLimit
	}
	if configureDesk || configurePhone {
		u.Exhausted = shouldBeExhausted
		changed = true
	}

	if changed {
		var entries []ledgerEntry
		if rolledOver {
			entries = append(entries, ledgerEntry{At: now, UserID: u.ID, Source: "reset", Note: "new cycle"})
		} else if u.UsedUnits != before.UsedUnits {
			entries = append(entries, ledgerEntry{At: now, UserID: u.ID, Units: u.UsedUnits - before.UsedUnits, Source: "sync"})
		}
		if err := s.store.saveAllowance(ctx, u, entries); err != nil {
			return entitlementResponse{}, err
		}
	} else if u.LastSyncAt.After(before.LastSyncAt) {
		// A sync that changed nothing still moves the clock. Losing that only
		// costs an extra gateway call, so a failure here is logged, not fatal.
		if err := s.store.touchSync(ctx, u.ID, u.LastSyncAt); err != nil {
			logf("warning: recording the sync time for %s: %v", u.ID, err)
		}
	}

	status := "active"
	if u.Exhausted {
		status = "exhausted"
	}
	return entitlementResponse{
		Vault:          s.entitlementVault(ctx, u, plan.Limits.VaultQuotaBytes),
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

// mintPhoneKey gives the user a phone key capped where a desk key minted now
// would be: this cycle's allowance, from zero spend. Callers hold syncMu, so
// two first grants can't both mint.
func (s *server) mintPhoneKey(ctx context.Context, u user) (user, error) {
	plan, config, err := s.store.plan(ctx, u.Plan)
	if errors.Is(err, errNotFound) {
		return user{}, errPlanMissing
	}
	if err != nil {
		return user{}, err
	}
	allowance := max(plan.AllowanceUnits+u.AdjustUnits, 0)
	limit := float64(allowance) / config.UnitsPerDollar
	key, err := s.gateway.mint(ctx, "thock-plus-"+u.ID+"-phone", limit)
	if err != nil {
		return user{}, err
	}
	revokeOrphan := func() {
		if err := s.gateway.revoke(context.Background(), key.Hash); err != nil {
			logf("warning: couldn't revoke phone key %s, minted for %s but never stored: %v", key.Hash, u.ID, err)
		}
	}
	if u.Exhausted {
		// Keys are minted enabled, and the loop only reconfigures on a
		// change, so a key born into a spent allowance is switched off here.
		if err := s.gateway.configure(ctx, key.Hash, limit, true); err != nil {
			revokeOrphan()
			return user{}, err
		}
	}
	err = s.store.setPhoneKey(ctx, u.ID, key)
	if errors.Is(err, errNotFound) {
		// Another instance stored a key first; hand out that one.
		revokeOrphan()
		current, err := s.store.userByID(ctx, u.ID)
		if err != nil {
			return user{}, err
		}
		if current.PhoneGateway.Hash == "" {
			return user{}, errors.New("the phone key couldn't be stored")
		}
		return current, nil
	}
	if err != nil {
		revokeOrphan()
		return user{}, err
	}
	u.PhoneGateway = key
	u.PhoneUsageBaselineUSD = 0
	return u, nil
}

// revokePhoneKeyLocked deletes the phone key at the gateway and forgets it,
// so the next grant mints a fresh one. With mustReachGateway the columns are
// kept when the gateway refuses, for a caller that can report the failure and
// be retried. Without it they are cleared regardless (see dropPhoneKey), and
// what the key spent this cycle is carried into the pool: otherwise pairing
// the phone again would hand back the allowance it used. A user being revoked
// has no cycle left to carry anything into. Callers hold syncMu.
func (s *server) revokePhoneKeyLocked(ctx context.Context, u user, mustReachGateway bool) error {
	if u.PhoneGateway.Hash == "" {
		return nil
	}
	if mustReachGateway {
		if err := s.gateway.revoke(ctx, u.PhoneGateway.Hash); err != nil {
			return err
		}
		return s.store.clearPhoneKey(ctx, u.ID, u.PhoneGateway.Hash, 0)
	}
	// Each step gets its own budget: a gateway that hangs on one call must
	// not use up the time the next one, or the database write, needs.
	carriedUSD := 0.0
	usageCtx, cancelUsage := context.WithTimeout(context.WithoutCancel(ctx), s.gatewayPatience)
	usage, err := s.gateway.usage(usageCtx, u.PhoneGateway.Hash)
	cancelUsage()
	if err != nil {
		logf("error: the spend on phone key %s of user %s couldn't be read before revoking it and is not counted this cycle: %v", u.PhoneGateway.Hash, u.ID, err)
	} else {
		carriedUSD = math.Max(0, usage-u.PhoneUsageBaselineUSD)
	}
	revokeCtx, cancelRevoke := context.WithTimeout(context.WithoutCancel(ctx), s.gatewayPatience)
	err = s.gateway.revoke(revokeCtx, u.PhoneGateway.Hash)
	cancelRevoke()
	if err != nil {
		logf("error: phone key %s of user %s couldn't be revoked at the gateway and is forgotten here; delete it by hand: %v", u.PhoneGateway.Hash, u.ID, err)
	}
	clearCtx, cancelClear := context.WithTimeout(context.WithoutCancel(ctx), 10*time.Second)
	defer cancelClear()
	return s.store.clearPhoneKey(clearCtx, u.ID, u.PhoneGateway.Hash, carriedUSD)
}

// dropPhoneKey ends the phone's key after its device row went or its vault
// lapsed. Those changes are already committed and can't be retried, so a
// gateway failure doesn't stop the key being forgotten: keeping it would hand
// a secret the old phone still holds to the next phone that pairs. The price
// is a key that may outlive its row, bounded by its cap and logged by hash.
func (s *server) dropPhoneKey(userID string) {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	u, err := s.store.userByID(ctx, userID)
	if err != nil {
		logf("error: reading user %s to revoke the phone key: %v", userID, err)
		return
	}
	if err := s.revokePhoneKeyLocked(ctx, u, false); err != nil {
		logf("error: forgetting the phone key of user %s: %v", userID, err)
	}
}

// revokeUser kills the gateway keys first so a revocation holds even if the
// database write fails afterwards. It holds syncMu so a grant can't mint a
// phone key for a user who is halfway out.
func (s *server) revokeUser(ctx context.Context, userID, note string) error {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()
	u, err := s.store.userByID(ctx, userID)
	if err != nil {
		return err
	}
	if u.Status == userRevoked {
		return errNotFound
	}
	// The phone key goes first and is forgotten at once, so a retry after a
	// failure further down doesn't ask the gateway to delete it twice.
	if err := s.revokePhoneKeyLocked(ctx, u, true); err != nil {
		return err
	}
	if err := s.gateway.revoke(ctx, u.Gateway.Hash); err != nil {
		return err
	}
	if err := s.store.revokeUser(ctx, userID, s.now(), note); err != nil {
		return err
	}
	// The vault lapses with the access; connected devices hear it at once,
	// as they do when the lapse comes from the admin route.
	if v, err := s.store.vaultByUser(ctx, userID); err == nil {
		s.feed.publish(v.ID, feedEvent{Kind: "vault", Data: map[string]any{"status": "lapsed"}})
	} else if !errors.Is(err, errNotFound) {
		logf("warning: announcing the lapse of user %s's vault: %v", userID, err)
	}
	return nil
}

// --- admin ---

func (s *server) handleAdminPlans(w http.ResponseWriter, r *http.Request) {
	config, err := s.store.settings(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't read the settings: "+err.Error())
		return
	}
	plans, err := s.store.listPlans(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't read the plans: "+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"units_per_dollar": config.UnitsPerDollar,
		"plans":            plans,
	})
}

func (s *server) handleAdminPutPlan(w http.ResponseWriter, r *http.Request) {
	var body plan
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&body); err != nil {
		writeError(w, http.StatusBadRequest, "The plan wasn't readable.")
		return
	}
	normalized, err := body.normalize(r.PathValue("id"))
	if err != nil {
		writeError(w, http.StatusUnprocessableEntity, "That plan isn't valid: "+err.Error())
		return
	}
	if err := s.store.upsertPlan(r.Context(), normalized); err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't save the plan: "+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, normalized)
}

func (s *server) handleAdminPutSettings(w http.ResponseWriter, r *http.Request) {
	var body settings
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&body); err != nil {
		writeError(w, http.StatusBadRequest, "The settings weren't readable.")
		return
	}
	if body.UnitsPerDollar <= 0 {
		writeError(w, http.StatusUnprocessableEntity, "units_per_dollar must be above zero.")
		return
	}
	if err := s.store.updateSettings(r.Context(), body.UnitsPerDollar); err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't save the settings: "+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, body)
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
	if !s.planExists(w, r, request.Plan) {
		return
	}
	if request.MaxUses < 0 {
		writeError(w, http.StatusUnprocessableEntity, "max_uses can't be negative; 0 means unlimited.")
		return
	}
	code, err := inviteCode()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't mint an invite code.")
		return
	}
	created := invite{Code: code, Plan: request.Plan, MaxUses: request.MaxUses, Note: request.Note, CreatedAt: s.now()}
	if err := s.store.createInvite(r.Context(), created); err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't save the invite: "+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, created)
}

// planExists answers the request itself when the plan is unknown or the
// lookup fails, so handlers can just return.
func (s *server) planExists(w http.ResponseWriter, r *http.Request, id string) bool {
	_, _, err := s.store.plan(r.Context(), id)
	switch {
	case errors.Is(err, errNotFound):
		writeError(w, http.StatusUnprocessableEntity, fmt.Sprintf("There is no plan %q.", id))
		return false
	case err != nil:
		writeError(w, http.StatusInternalServerError, "Couldn't read the plans: "+err.Error())
		return false
	}
	return true
}

func (s *server) handleAdminListInvites(w http.ResponseWriter, r *http.Request) {
	invites, err := s.store.listInvites(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't read the invites: "+err.Error())
		return
	}
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
	// Empty until the phone asks for its first grant.
	PhoneGatewayKeyHash string `json:"phone_gateway_key_hash,omitempty"`
}

func (s *server) handleAdminListUsers(w http.ResponseWriter, r *http.Request) {
	stored, err := s.store.listUsers(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't read the users: "+err.Error())
		return
	}
	users := make([]adminUser, 0, len(stored))
	for _, u := range stored {
		users = append(users, adminUser{
			ID: u.ID, Plan: u.Plan, Device: u.Device, InviteCode: u.InviteCode, Status: u.Status,
			CreatedAt: u.CreatedAt, CycleStartedAt: u.CycleStartedAt, UsedUnits: u.UsedUnits,
			AdjustUnits: u.AdjustUnits, Exhausted: u.Exhausted, GatewayKeyHash: u.Gateway.Hash,
			PhoneGatewayKeyHash: u.PhoneGateway.Hash,
		})
	}
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
	if request.Plan != "" && !s.planExists(w, r, request.Plan) {
		return
	}
	err := s.changeAllowance(r.Context(), userID, request)
	var refusal *refusalError
	if errors.As(err, &refusal) {
		writeErrorCode(w, refusal.status, refusal.code, refusal.message)
		return
	}
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't update the allowance: "+err.Error())
		return
	}
	entitlement, err := s.entitlementFor(r.Context(), userID)
	if err != nil {
		writeError(w, http.StatusBadGateway, "The allowance was saved but couldn't be applied at the gateway: "+err.Error())
		return
	}
	entitlement.Gateway = nil
	writeJSON(w, http.StatusOK, entitlement)
}

// changeAllowance records an admin change. It holds syncMu so a reset reads
// the usage of exactly the keys whose baselines it then stores: no phone key
// is minted or revoked in between.
func (s *server) changeAllowance(ctx context.Context, userID string, request allowanceRequest) error {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()
	u, err := s.store.userByID(ctx, userID)
	if errors.Is(err, errNotFound) {
		return refuse(http.StatusNotFound, "not_found", "No such user.")
	}
	if err != nil {
		return refuse(http.StatusInternalServerError, "internal", "Couldn't read the user: "+err.Error())
	}
	change := allowanceChange{Plan: request.Plan, Reset: request.Reset, AdjustUnits: request.AdjustUnits, Note: request.Note, At: s.now()}
	if request.Reset {
		usage, phoneUsage, err := s.keyUsage(ctx, u)
		if err != nil {
			return refuse(http.StatusBadGateway, "upstream", "Couldn't read the gateway usage to reset from: "+err.Error())
		}
		change.BaselineUSD = usage
		change.PhoneBaselineUSD = phoneUsage
	}
	return s.store.applyAllowance(ctx, userID, change)
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
	entries, err := s.store.ledgerFor(r.Context(), r.PathValue("id"))
	if err != nil {
		writeError(w, http.StatusInternalServerError, "Couldn't read the ledger: "+err.Error())
		return
	}
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
		u, err := s.store.userByCredential(r.Context(), hashCredential(credential))
		if errors.Is(err, errNotFound) {
			writeError(w, http.StatusUnauthorized, "That Thock Plus connection is no longer valid. Connect again with an invite code.")
			return
		}
		if err != nil {
			logf("error: looking up a credential: %v", err)
			writeError(w, http.StatusInternalServerError, "Couldn't check your connection right now. Try again.")
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

// writeError answers with the sentence and the code the status implies;
// handlers that need a more specific code use writeErrorCode.
func writeError(w http.ResponseWriter, status int, message string) {
	writeErrorCode(w, status, defaultErrorCode(status), message)
}

func writeErrorCode(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]string{"error": message, "code": code})
}

func defaultErrorCode(status int) string {
	switch status {
	case http.StatusBadRequest:
		return "bad_request"
	case http.StatusUnauthorized:
		return "unauthorized"
	case http.StatusForbidden:
		return "revoked"
	case http.StatusNotFound:
		return "not_found"
	case http.StatusGone:
		return "gone"
	case http.StatusUnprocessableEntity:
		return "invalid"
	case http.StatusServiceUnavailable:
		return "unavailable"
	case http.StatusBadGateway:
		return "upstream"
	default:
		return "internal"
	}
}
