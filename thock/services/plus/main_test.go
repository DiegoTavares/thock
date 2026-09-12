package main

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

// The plan every test connects under, on top of the seeded catalog.
var testPlan = plan{
	ID:             "test",
	Name:           "Dev",
	AllowanceUnits: 500,
	CycleDays:      30,
	Models:         modelTiers{Default: "google/gemini-2.5-flash", Fast: "google/gemini-2.5-flash-lite"},
	Limits:         planLimits{WarnAtPercent: 80, MaxTurnsPerSession: 50},
}

type harness struct {
	t       *testing.T
	server  *server
	gateway *fakeGateway
	clock   time.Time
	http    *httptest.Server
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	store, err := openStore(context.Background(), freshDatabaseURL(t))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(store.close)
	if err := store.upsertPlan(context.Background(), testPlan); err != nil {
		t.Fatal(err)
	}
	gw := newFakeGateway()
	s := newServer(store, gw, "admin-secret")
	h := &harness{t: t, server: s, gateway: gw, clock: time.Date(2026, 9, 11, 12, 0, 0, 0, time.UTC)}
	s.now = func() time.Time { return h.clock }
	h.http = httptest.NewServer(s.routes())
	t.Cleanup(h.http.Close)
	return h
}

func (h *harness) call(method, path, token string, body any) (int, map[string]any) {
	h.t.Helper()
	var reader *bytes.Reader
	if body != nil {
		raw, err := json.Marshal(body)
		if err != nil {
			h.t.Fatal(err)
		}
		reader = bytes.NewReader(raw)
	} else {
		reader = bytes.NewReader(nil)
	}
	request, err := http.NewRequest(method, h.http.URL+path, reader)
	if err != nil {
		h.t.Fatal(err)
	}
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		h.t.Fatal(err)
	}
	defer response.Body.Close()
	var decoded map[string]any
	if response.StatusCode != http.StatusNoContent {
		if err := json.NewDecoder(response.Body).Decode(&decoded); err != nil {
			h.t.Fatalf("%s %s: undecodable body: %v", method, path, err)
		}
	}
	return response.StatusCode, decoded
}

func (h *harness) invite() string {
	h.t.Helper()
	status, body := h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "test", "max_uses": 1, "note": "test"})
	if status != 200 {
		h.t.Fatalf("invite: %d %v", status, body)
	}
	return body["code"].(string)
}

func (h *harness) connect(code string) (credential string, entitlement map[string]any) {
	h.t.Helper()
	status, body := h.call("POST", "/v1/connect", "", map[string]any{"invite_code": code, "device": "test mac"})
	if status != 200 {
		h.t.Fatalf("connect: %d %v", status, body)
	}
	return body["credential"].(string), body["entitlement"].(map[string]any)
}

func (h *harness) entitlement(credential string) (int, map[string]any) {
	h.t.Helper()
	return h.call("GET", "/v1/entitlement", credential, nil)
}

func (h *harness) user(credential string) user {
	h.t.Helper()
	u, err := h.server.store.userByCredential(context.Background(), hashCredential(credential))
	if err != nil {
		h.t.Fatalf("user not found: %v", err)
	}
	return u
}

func (h *harness) keyHash(credential string) string {
	h.t.Helper()
	return h.user(credential).Gateway.Hash
}

func TestConnectMintsACappedKeyAndReportsTheAllowance(t *testing.T) {
	h := newHarness(t)
	code := h.invite()
	if len(code) != len("THOCK-XXXX-XXXX") {
		t.Fatalf("invite code shape: %q", code)
	}
	credential, entitlement := h.connect(code)
	if entitlement["status"] != "active" || entitlement["remaining_units"].(float64) != 500 {
		t.Fatalf("fresh entitlement: %v", entitlement)
	}
	gateway := entitlement["gateway"].(map[string]any)
	if gateway["provider"] != "openrouter" || gateway["api_key"] == "" {
		t.Fatalf("gateway grant: %v", gateway)
	}
	models := gateway["models"].(map[string]any)
	if models["default"] != "google/gemini-2.5-flash" || models["fast"] != "google/gemini-2.5-flash-lite" {
		t.Fatalf("model tiers: %v", models)
	}
	key, ok := h.gateway.lookup(h.keyHash(credential))
	if !ok || key.LimitUSD != 5 || key.Disabled {
		t.Fatalf("gateway key should be capped at $5 and enabled: %+v", key)
	}

	// The single-use invite is spent.
	status, body := h.call("POST", "/v1/connect", "", map[string]any{"invite_code": code})
	if status != http.StatusGone {
		t.Fatalf("reused invite: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/connect", "", map[string]any{"invite_code": "THOCK-NOPE-NOPE"})
	if status != http.StatusNotFound || body["error"] == "" {
		t.Fatalf("unknown invite: %d %v", status, body)
	}
}

func TestUsageCountsDownAndExhaustionDisablesTheKey(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	hash := h.keyHash(credential)

	if err := h.gateway.spend(hash, 4.10); err != nil {
		t.Fatal(err)
	}
	// Inside the sync interval the cached balance is served.
	_, entitlement := h.entitlement(credential)
	if entitlement["used_units"].(float64) != 0 {
		t.Fatalf("expected the cached balance, got %v", entitlement)
	}
	h.clock = h.clock.Add(time.Minute)
	_, entitlement = h.entitlement(credential)
	if entitlement["used_units"].(float64) != 410 || entitlement["remaining_units"].(float64) != 90 || entitlement["status"] != "active" {
		t.Fatalf("after spending $4.10: %v", entitlement)
	}

	if err := h.gateway.spend(hash, 0.95); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	_, entitlement = h.entitlement(credential)
	if entitlement["status"] != "exhausted" || entitlement["remaining_units"].(float64) != 0 {
		t.Fatalf("after overspending: %v", entitlement)
	}
	key, _ := h.gateway.lookup(hash)
	if !key.Disabled {
		t.Fatal("an exhausted allowance must disable the gateway key")
	}
	if err := h.gateway.spend(hash, 0.01); err == nil {
		t.Fatal("a disabled key must refuse traffic")
	}

	// A top-up brings it back.
	userID := h.user(credential).ID
	status, body := h.call("POST", "/admin/users/"+userID+"/allowance", "admin-secret", map[string]any{"adjust_units": 200, "note": "top-up"})
	if status != 200 || body["status"] != "active" || body["remaining_units"].(float64) != 195 {
		t.Fatalf("top-up: %d %v", status, body)
	}
	key, _ = h.gateway.lookup(hash)
	if key.Disabled || key.LimitUSD != 7 {
		t.Fatalf("top-up should re-enable and raise the cap to $7: %+v", key)
	}

	// A reset starts a fresh cycle from the current gateway usage.
	status, body = h.call("POST", "/admin/users/"+userID+"/allowance", "admin-secret", map[string]any{"reset": true})
	if status != 200 || body["used_units"].(float64) != 0 || body["remaining_units"].(float64) != 500 {
		t.Fatalf("reset: %d %v", status, body)
	}
	key, _ = h.gateway.lookup(hash)
	if key.LimitUSD < 10.04 || key.LimitUSD > 10.06 {
		t.Fatalf("reset should cap at spent-so-far plus a full allowance: %+v", key)
	}

	request, err := http.NewRequest("GET", h.http.URL+"/admin/users/"+userID+"/ledger", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer admin-secret")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	var ledger []ledgerEntry
	if err := json.NewDecoder(response.Body).Decode(&ledger); err != nil || response.StatusCode != 200 {
		t.Fatalf("ledger: %d %v", response.StatusCode, err)
	}
	sources := map[string]int{}
	for _, entry := range ledger {
		sources[entry.Source]++
	}
	if sources["connect"] != 1 || sources["sync"] != 2 || sources["adjust"] != 1 || sources["reset"] != 1 {
		t.Fatalf("ledger sources: %v", sources)
	}
}

func TestCycleRolloverResetsTheBalance(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	hash := h.keyHash(credential)
	if err := h.gateway.spend(hash, 3); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(31 * 24 * time.Hour)
	_, entitlement := h.entitlement(credential)
	if entitlement["used_units"].(float64) != 0 || entitlement["remaining_units"].(float64) != 500 {
		t.Fatalf("after rollover: %v", entitlement)
	}
	ends, err := time.Parse(time.RFC3339, entitlement["cycle_ends_at"].(string))
	if err != nil || !ends.After(h.clock) {
		t.Fatalf("cycle end should be in the future: %v %v", entitlement["cycle_ends_at"], err)
	}
	key, _ := h.gateway.lookup(hash)
	if key.LimitUSD != 8 {
		t.Fatalf("the cap should move with the new cycle: %+v", key)
	}
}

func TestRevocationLocksTheUserOutAndKillsTheKey(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	hash := h.keyHash(credential)
	userID := h.user(credential).ID
	status, _ := h.call("POST", "/admin/users/"+userID+"/revoke", "admin-secret", nil)
	if status != http.StatusNoContent {
		t.Fatalf("revoke: %d", status)
	}
	key, _ := h.gateway.lookup(hash)
	if !key.Revoked {
		t.Fatal("revocation must delete the gateway key")
	}
	status, body := h.entitlement(credential)
	if status != http.StatusForbidden || body["error"] == "" {
		t.Fatalf("revoked entitlement: %d %v", status, body)
	}

	// Disconnecting from the app is the same thing, self-served.
	credential2, _ := h.connect(h.invite())
	status, _ = h.call("POST", "/v1/disconnect", credential2, nil)
	if status != http.StatusNoContent {
		t.Fatalf("disconnect: %d", status)
	}
	status, _ = h.entitlement(credential2)
	if status != http.StatusForbidden {
		t.Fatalf("after disconnect: %d", status)
	}
	status, _ = h.entitlement("tpk_not_a_credential")
	if status != http.StatusUnauthorized {
		t.Fatalf("bogus credential: %d", status)
	}
}

func TestPlanEditsApplyOnTheNextRead(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	status, body := h.call("PUT", "/admin/plans/test", "admin-secret", map[string]any{
		"name": "Dev", "allowance_units": 900, "models": map[string]any{"default": "x/y"},
	})
	if status != 200 || body["cycle_days"].(float64) != 30 {
		t.Fatalf("edit: %d %v", status, body)
	}
	_, entitlement := h.entitlement(credential)
	if entitlement["allowance_units"].(float64) != 900 || entitlement["remaining_units"].(float64) != 900 {
		t.Fatalf("edited allowance should apply on the next read: %v", entitlement)
	}
	models := entitlement["gateway"].(map[string]any)["models"].(map[string]any)
	if models["default"] != "x/y" || models["fast"] != "x/y" {
		t.Fatalf("fast should fall back to default: %v", models)
	}

	// An invalid edit changes nothing.
	status, body = h.call("PUT", "/admin/plans/test", "admin-secret", map[string]any{"allowance_units": -1, "models": map[string]any{"default": "x/y"}})
	if status != http.StatusUnprocessableEntity || body["error"] == "" {
		t.Fatalf("negative allowance: %d %v", status, body)
	}
	status, body = h.call("PUT", "/admin/plans/test", "admin-secret", map[string]any{"id": "other", "models": map[string]any{"default": "x/y"}})
	if status != http.StatusUnprocessableEntity {
		t.Fatalf("mismatched id: %d %v", status, body)
	}
	_, entitlement = h.entitlement(credential)
	if entitlement["allowance_units"].(float64) != 900 {
		t.Fatalf("a rejected edit must not change anything: %v", entitlement)
	}

	// The seeded catalog is there alongside, and the listing shows the edit.
	status, body = h.call("GET", "/admin/plans", "admin-secret", nil)
	if status != 200 || body["units_per_dollar"].(float64) != 100 {
		t.Fatalf("plans: %d %v", status, body)
	}
	ids := map[string]float64{}
	for _, entry := range body["plans"].([]any) {
		p := entry.(map[string]any)
		ids[p["id"].(string)] = p["allowance_units"].(float64)
	}
	if ids["test"] != 900 || ids["plus"] != 1000 || ids["dev"] != 300 {
		t.Fatalf("plan listing: %v", ids)
	}

	// units_per_dollar reprices the next mint.
	status, body = h.call("PUT", "/admin/settings", "admin-secret", map[string]any{"units_per_dollar": 1000})
	if status != 200 {
		t.Fatalf("settings: %d %v", status, body)
	}
	credential2, _ := h.connect(h.invite())
	key, _ := h.gateway.lookup(h.keyHash(credential2))
	if key.LimitUSD != 0.9 {
		t.Fatalf("900 units at 1000 per dollar should cap at $0.90: %+v", key)
	}
}

func TestInvitesAreSpentAtomically(t *testing.T) {
	h := newHarness(t)
	status, body := h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "test", "max_uses": 2, "note": "pair"})
	if status != 200 {
		t.Fatalf("invite: %d %v", status, body)
	}
	code := body["code"].(string)
	results := make(chan int, 3)
	for range 3 {
		go func() {
			status, _ := h.call("POST", "/v1/connect", "", map[string]any{"invite_code": code})
			results <- status
		}()
	}
	counts := map[int]int{}
	for range 3 {
		counts[<-results]++
	}
	if counts[200] != 2 || counts[http.StatusGone] != 1 {
		t.Fatalf("two of three racing connects should win: %v", counts)
	}
	spent, err := h.server.store.inviteByCode(context.Background(), code)
	if err != nil || spent.Uses != 2 {
		t.Fatalf("the invite should record exactly two uses: %+v %v", spent, err)
	}
	users, err := h.server.store.listUsers(context.Background())
	if err != nil || len(users) != 2 {
		t.Fatalf("expected two users, got %d (%v)", len(users), err)
	}
	if !h.gateway.mintedAndRevoked(3, 1) {
		t.Fatal("the loser's minted key must be revoked again")
	}
}

func TestStateSurvivesARestart(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	reopened, err := openStore(context.Background(), h.server.store.pool.Config().ConnString())
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.close()
	if _, err := reopened.userByCredential(context.Background(), hashCredential(credential)); err != nil {
		t.Fatalf("the user should be there after a reopen: %v", err)
	}
	applied, err := migrate(context.Background(), reopened.pool)
	if err != nil || len(applied) != 0 {
		t.Fatalf("a second migrate should apply nothing: %v %v", applied, err)
	}
}

func TestAdminEndpointsNeedTheToken(t *testing.T) {
	h := newHarness(t)
	status, _ := h.call("POST", "/admin/invites", "wrong", map[string]any{"plan": "dev"})
	if status != http.StatusUnauthorized {
		t.Fatalf("wrong admin token: %d", status)
	}
	status, body := h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "nope"})
	if status != http.StatusUnprocessableEntity || body["error"] == "" {
		t.Fatalf("unknown plan: %d %v", status, body)
	}
	response, err := http.Get(h.http.URL + "/health")
	if err != nil || response.StatusCode != 200 {
		t.Fatalf("health: %v %v", response, err)
	}
	response.Body.Close()
}
