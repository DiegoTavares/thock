package main

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

const testPlans = `{
  "version": 1,
  "units_per_dollar": 100,
  "plans": {
    "dev": {
      "name": "Dev",
      "allowance_units": 500,
      "cycle_days": 30,
      "models": {"default": "google/gemini-2.5-flash", "fast": "google/gemini-2.5-flash-lite"},
      "limits": {"warn_at_percent": 80, "max_turns_per_session": 50}
    }
  }
}`

type harness struct {
	t       *testing.T
	server  *server
	gateway *fakeGateway
	plans   string
	clock   time.Time
	http    *httptest.Server
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	dir := t.TempDir()
	plansPath := filepath.Join(dir, "plans.json")
	if err := os.WriteFile(plansPath, []byte(testPlans), 0o600); err != nil {
		t.Fatal(err)
	}
	catalog, err := loadPlanCatalog(plansPath)
	if err != nil {
		t.Fatal(err)
	}
	store, err := openStore(filepath.Join(dir, "data", "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	gw := newFakeGateway()
	s := newServer(catalog, store, gw, "admin-secret")
	h := &harness{t: t, server: s, gateway: gw, plans: plansPath, clock: time.Date(2026, 9, 11, 12, 0, 0, 0, time.UTC)}
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
	status, body := h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "dev", "max_uses": 1, "note": "test"})
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

func (h *harness) keyHash(credential string) string {
	h.t.Helper()
	u, ok := h.server.store.userByCredential(hashCredential(credential))
	if !ok {
		h.t.Fatal("user not found")
	}
	return u.Gateway.Hash
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
	var userID string
	h.server.store.read(func(state state) {
		for id := range state.Users {
			userID = id
		}
	})
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
	var userID string
	h.server.store.read(func(state state) {
		for id := range state.Users {
			userID = id
		}
	})
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

func TestPlansAreHotReloadedFromTheFile(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	edited := []byte(`{"plans": {"dev": {"name": "Dev", "allowance_units": 900, "models": {"default": "x/y"}}}}`)
	// Bump mtime explicitly: some filesystems round it to the second.
	if err := os.WriteFile(h.plans, edited, 0o600); err != nil {
		t.Fatal(err)
	}
	future := time.Now().Add(2 * time.Second)
	if err := os.Chtimes(h.plans, future, future); err != nil {
		t.Fatal(err)
	}
	_, entitlement := h.entitlement(credential)
	if entitlement["allowance_units"].(float64) != 900 || entitlement["remaining_units"].(float64) != 900 {
		t.Fatalf("edited allowance should apply on the next read: %v", entitlement)
	}
	models := entitlement["gateway"].(map[string]any)["models"].(map[string]any)
	if models["default"] != "x/y" || models["fast"] != "x/y" {
		t.Fatalf("fast should fall back to default: %v", models)
	}

	// A broken edit keeps the last good plans.
	if err := os.WriteFile(h.plans, []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	later := future.Add(2 * time.Second)
	if err := os.Chtimes(h.plans, later, later); err != nil {
		t.Fatal(err)
	}
	_, entitlement = h.entitlement(credential)
	if entitlement["allowance_units"].(float64) != 900 {
		t.Fatalf("a broken plans file must not change anything: %v", entitlement)
	}
	status, body := h.call("POST", "/admin/plans/reload", "admin-secret", nil)
	if status != http.StatusUnprocessableEntity || body["error"] == "" {
		t.Fatalf("explicit reload of a broken file should say so: %d %v", status, body)
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
}

func TestStateSurvivesARestart(t *testing.T) {
	h := newHarness(t)
	credential, _ := h.connect(h.invite())
	reopened, err := openStore(h.server.store.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := reopened.userByCredential(hashCredential(credential)); !ok {
		t.Fatal("the credential index should be rebuilt from the file")
	}
}
