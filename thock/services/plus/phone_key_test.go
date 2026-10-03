package main

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"
)

// The phone's agent grant and the one allowance pool behind it
// (thock/specs/v35-phone-ask.md §5.1 and §7, backend list).

func (h *syncHarness) grant(token string) (int, map[string]any) {
	h.t.Helper()
	return h.call("GET", "/v1/vault/agent", token, nil)
}

func (h *syncHarness) mustGrant(token string) map[string]any {
	h.t.Helper()
	status, body := h.grant(token)
	if status != http.StatusOK {
		h.t.Fatalf("grant: %d %v", status, body)
	}
	return body
}

func grantedKey(grant map[string]any) string {
	return grant["gateway"].(map[string]any)["api_key"].(string)
}

// countingGateway records every call that reaches the gateway and can be
// told to refuse revocations.
type countingGateway struct {
	gateway
	mu         sync.Mutex
	calls      map[string]int
	failRevoke bool
	// Makes usage and revoke wait for their context to give up.
	hang bool
}

func (g *countingGateway) hanging(ctx context.Context) bool {
	g.mu.Lock()
	hang := g.hang
	g.mu.Unlock()
	if hang {
		<-ctx.Done()
	}
	return hang
}

func (h *harness) countGatewayCalls() *countingGateway {
	counting := &countingGateway{gateway: h.gateway, calls: map[string]int{}}
	h.server.gateway = counting
	return counting
}

func (g *countingGateway) count(name string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.calls[name]++
}

func (g *countingGateway) counted(name string) int {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.calls[name]
}

func (g *countingGateway) mint(ctx context.Context, name string, limitUSD float64) (gatewayKey, error) {
	g.count("mint")
	return g.gateway.mint(ctx, name, limitUSD)
}

func (g *countingGateway) usage(ctx context.Context, hash string) (float64, error) {
	g.count("usage")
	if g.hanging(ctx) {
		return 0, ctx.Err()
	}
	return g.gateway.usage(ctx, hash)
}

func (g *countingGateway) configure(ctx context.Context, hash string, limitUSD float64, disabled bool) error {
	g.count("configure")
	return g.gateway.configure(ctx, hash, limitUSD, disabled)
}

func (g *countingGateway) revoke(ctx context.Context, hash string) error {
	g.count("revoke")
	g.mu.Lock()
	fail := g.failRevoke
	g.mu.Unlock()
	if fail {
		return errors.New("the gateway is unreachable")
	}
	if g.hanging(ctx) {
		return ctx.Err()
	}
	return g.gateway.revoke(ctx, hash)
}

func TestTheGrantMintsOnePhoneKeyAndKeepsIt(t *testing.T) {
	h := newSyncHarness(t)
	deskKey := h.user(h.desk).Gateway

	grant := h.mustGrant(h.phone)
	if grant["status"] != "active" || grant["allowance_units"].(float64) != 500 || grant["used_units"].(float64) != 0 ||
		grant["remaining_units"].(float64) != 500 || grant["warn_at_percent"].(float64) != 80 {
		t.Fatalf("fresh grant: %v", grant)
	}
	ends, err := time.Parse(time.RFC3339, grant["cycle_ends_at"].(string))
	if err != nil || !ends.Equal(h.clock.Add(30*24*time.Hour)) {
		t.Fatalf("cycle end: %v %v", grant["cycle_ends_at"], err)
	}
	gateway := grant["gateway"].(map[string]any)
	models := gateway["models"].(map[string]any)
	if gateway["provider"] != "openrouter" || gateway["base_url"] != "https://openrouter.ai/api/v1" ||
		models["default"] != testPlan.Models.Default || models["fast"] != testPlan.Models.Fast {
		t.Fatalf("gateway block: %v", gateway)
	}
	for field := range grant {
		switch field {
		case "status", "allowance_units", "used_units", "remaining_units", "warn_at_percent", "cycle_ends_at", "gateway":
		default:
			t.Errorf("the grant carries an unexpected field %q", field)
		}
	}

	phoneKey := h.user(h.desk).PhoneGateway
	if phoneKey.Hash == "" || phoneKey.Hash == deskKey.Hash || phoneKey.Secret == deskKey.Secret {
		t.Fatalf("the phone needs a key of its own: %+v", phoneKey)
	}
	if grantedKey(grant) != phoneKey.Secret {
		t.Fatalf("the grant should carry the phone key, got %v", gateway["api_key"])
	}
	raw, err := json.Marshal(grant)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), deskKey.Secret) {
		t.Fatal("the desk key must never reach the phone")
	}
	minted, _ := h.gateway.lookup(phoneKey.Hash)
	if minted.LimitUSD != 5 || minted.Disabled {
		t.Fatalf("the phone key is capped like the desk key: %+v", minted)
	}

	again := h.mustGrant(h.phone)
	if grantedKey(again) != phoneKey.Secret {
		t.Fatalf("a second grant should return the same key: %v", again)
	}
	if !h.gateway.mintedAndRevoked(2, 0) {
		t.Fatal("exactly one desk key and one phone key should exist")
	}
	_, entitlement := h.entitlement(h.desk)
	if entitlement["gateway"].(map[string]any)["api_key"] != deskKey.Secret {
		t.Fatalf("the desk keeps its own key: %v", entitlement)
	}
}

func TestConcurrentFirstGrantsMintOneKey(t *testing.T) {
	h := newSyncHarness(t)
	const callers = 6
	keys := make([]string, callers)
	var group sync.WaitGroup
	for i := range callers {
		group.Add(1)
		go func() {
			defer group.Done()
			request, err := http.NewRequest("GET", h.http.URL+"/v1/vault/agent", nil)
			if err != nil {
				return
			}
			request.Header.Set("Authorization", "Bearer "+h.phone)
			response, err := http.DefaultClient.Do(request)
			if err != nil {
				return
			}
			defer response.Body.Close()
			var grant struct {
				Gateway struct {
					APIKey string `json:"api_key"`
				} `json:"gateway"`
			}
			if err := json.NewDecoder(response.Body).Decode(&grant); err == nil {
				keys[i] = grant.Gateway.APIKey
			}
		}()
	}
	group.Wait()
	for _, key := range keys {
		if key == "" || key != keys[0] {
			t.Fatalf("every caller should get the one phone key: %v", keys)
		}
	}
	if !h.gateway.mintedAndRevoked(2, 0) {
		t.Fatal("racing first grants must not mint more than one phone key")
	}
}

func TestTheGrantIsOnlyForThePhoneOfALiveVault(t *testing.T) {
	h := newSyncHarness(t)
	status, body := h.grant(h.desk)
	if status != http.StatusForbidden || body["code"] != "role_forbidden" {
		t.Fatalf("a desk asking for the phone's grant: %d %v", status, body)
	}

	userID := h.user(h.desk).ID
	if status, _ := h.call("POST", "/admin/users/"+userID+"/vault/lapse", "admin-secret", map[string]any{"lapsed": true}); status != http.StatusNoContent {
		t.Fatalf("lapse: %d", status)
	}
	status, body = h.grant(h.phone)
	if status != http.StatusForbidden || body["code"] != "plus_lapsed" || body["error"] == "" {
		t.Fatalf("a lapsed vault's grant: %d %v", status, body)
	}
	if !h.gateway.mintedAndRevoked(1, 0) {
		t.Fatal("a refused grant must not mint a key")
	}
}

func TestSpendOnEitherKeyDrawsOnOnePool(t *testing.T) {
	h := newSyncHarness(t)
	h.mustGrant(h.phone)
	u := h.user(h.desk)
	deskHash, phoneHash := u.Gateway.Hash, u.PhoneGateway.Hash

	if err := h.gateway.spend(deskHash, 1.00); err != nil {
		t.Fatal(err)
	}
	if err := h.gateway.spend(phoneHash, 2.50); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	grant := h.mustGrant(h.phone)
	if grant["used_units"].(float64) != 350 || grant["remaining_units"].(float64) != 150 || grant["status"] != "active" {
		t.Fatalf("the grant after $1.00 at the desk and $2.50 on the phone: %v", grant)
	}
	_, entitlement := h.entitlement(h.desk)
	if entitlement["used_units"].(float64) != 350 || entitlement["remaining_units"].(float64) != 150 {
		t.Fatalf("the desk should see the same balance: %v", entitlement)
	}

	// The phone alone spends the rest; the desk's poll is what notices.
	if err := h.gateway.spend(phoneHash, 1.60); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	_, entitlement = h.entitlement(h.desk)
	if entitlement["status"] != "exhausted" || entitlement["used_units"].(float64) != 510 || entitlement["remaining_units"].(float64) != 0 {
		t.Fatalf("after the pool ran dry: %v", entitlement)
	}
	for name, hash := range map[string]string{"desk": deskHash, "phone": phoneHash} {
		if key, _ := h.gateway.lookup(hash); !key.Disabled {
			t.Fatalf("an exhausted allowance must disable the %s key: %+v", name, key)
		}
	}
	grant = h.mustGrant(h.phone)
	if grant["status"] != "exhausted" || grant["remaining_units"].(float64) != 0 || grantedKey(grant) != u.PhoneGateway.Secret {
		t.Fatalf("an exhausted grant still carries the (disabled) key: %v", grant)
	}

	status, body := h.call("POST", "/admin/users/"+u.ID+"/allowance", "admin-secret", map[string]any{"adjust_units": 200, "note": "top-up"})
	if status != http.StatusOK || body["status"] != "active" || body["remaining_units"].(float64) != 190 {
		t.Fatalf("top-up: %d %v", status, body)
	}
	for name, hash := range map[string]string{"desk": deskHash, "phone": phoneHash} {
		if key, _ := h.gateway.lookup(hash); key.Disabled || key.LimitUSD != 7 {
			t.Fatalf("a top-up should re-enable the %s key and raise its cap to $7: %+v", name, key)
		}
	}
	grant = h.mustGrant(h.phone)
	if grant["status"] != "active" || grant["allowance_units"].(float64) != 700 || grant["remaining_units"].(float64) != 190 {
		t.Fatalf("the grant after the top-up: %v", grant)
	}
}

func TestAPhoneKeyMintedIntoASpentAllowanceStartsDisabled(t *testing.T) {
	h := newSyncHarness(t)
	if err := h.gateway.spend(h.keyHash(h.desk), 5); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	if _, entitlement := h.entitlement(h.desk); entitlement["status"] != "exhausted" {
		t.Fatalf("the desk should have spent everything: %v", entitlement)
	}
	grant := h.mustGrant(h.phone)
	if grant["status"] != "exhausted" {
		t.Fatalf("a first grant into a spent allowance: %v", grant)
	}
	if key, _ := h.gateway.lookup(h.user(h.desk).PhoneGateway.Hash); !key.Disabled {
		t.Fatalf("the new phone key must not work until there is a balance: %+v", key)
	}
}

func TestANewCycleResetsBothBaselines(t *testing.T) {
	h := newSyncHarness(t)
	h.mustGrant(h.phone)
	u := h.user(h.desk)
	deskHash, phoneHash := u.Gateway.Hash, u.PhoneGateway.Hash
	if err := h.gateway.spend(deskHash, 3); err != nil {
		t.Fatal(err)
	}
	if err := h.gateway.spend(phoneHash, 2); err != nil {
		t.Fatal(err)
	}

	h.clock = h.clock.Add(31 * 24 * time.Hour)
	grant := h.mustGrant(h.phone)
	if grant["status"] != "active" || grant["used_units"].(float64) != 0 || grant["remaining_units"].(float64) != 500 {
		t.Fatalf("after rollover: %v", grant)
	}
	u = h.user(h.desk)
	if u.UsageBaselineUSD != 3 || u.PhoneUsageBaselineUSD != 2 {
		t.Fatalf("each key's baseline should be its own spend so far: desk %v, phone %v", u.UsageBaselineUSD, u.PhoneUsageBaselineUSD)
	}
	deskKey, _ := h.gateway.lookup(deskHash)
	phoneKey, _ := h.gateway.lookup(phoneHash)
	if deskKey.LimitUSD != 8 || phoneKey.LimitUSD != 7 || deskKey.Disabled || phoneKey.Disabled {
		t.Fatalf("each cap is its own baseline plus the allowance: desk %+v, phone %+v", deskKey, phoneKey)
	}

	if err := h.gateway.spend(phoneHash, 1); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	if _, entitlement := h.entitlement(h.desk); entitlement["used_units"].(float64) != 100 {
		t.Fatalf("only spend since the rollover counts: %v", entitlement)
	}

	// An admin reset is a rollover by hand and moves both baselines too.
	status, body := h.call("POST", "/admin/users/"+u.ID+"/allowance", "admin-secret", map[string]any{"reset": true})
	if status != http.StatusOK || body["used_units"].(float64) != 0 || body["remaining_units"].(float64) != 500 {
		t.Fatalf("reset: %d %v", status, body)
	}
	u = h.user(h.desk)
	phoneKey, _ = h.gateway.lookup(phoneHash)
	if u.UsageBaselineUSD != 3 || u.PhoneUsageBaselineUSD != 3 || phoneKey.LimitUSD != 8 {
		t.Fatalf("after the reset: desk baseline %v, phone baseline %v, phone key %+v", u.UsageBaselineUSD, u.PhoneUsageBaselineUSD, phoneKey)
	}
}

func TestPairingAgainDoesNotHandBackWhatThePhoneSpent(t *testing.T) {
	h := newSyncHarness(t)
	h.mustGrant(h.phone)
	u := h.user(h.desk)
	if err := h.gateway.spend(u.Gateway.Hash, 1.00); err != nil {
		t.Fatal(err)
	}
	if err := h.gateway.spend(u.PhoneGateway.Hash, 2.50); err != nil {
		t.Fatal(err)
	}

	next := h.pairPhone("Replacement iPhone", "")
	h.clock = h.clock.Add(time.Minute)
	grant := h.mustGrant(next)
	if grantedKey(grant) == u.PhoneGateway.Secret {
		t.Fatal("the replacement phone was handed the old phone's key")
	}
	if grant["used_units"].(float64) != 350 || grant["remaining_units"].(float64) != 150 {
		t.Fatalf("the old phone's spend should still count after pairing again: %v", grant)
	}
	// The desk key's own cap tightens by what the old phone spent.
	if key, _ := h.gateway.lookup(u.Gateway.Hash); key.LimitUSD != 2.50 {
		t.Fatalf("the desk key's cap after the old phone's $2.50: %+v", key)
	}
}

func TestPairingAgainAtOnceCannotRefillASpentAllowance(t *testing.T) {
	h := newSyncHarness(t)
	h.mustGrant(h.phone)
	u := h.user(h.desk)
	phone := h.phone
	// No clock advance anywhere: every grant lands inside the sync interval
	// of the one before it, where a cached balance would still say "unspent".
	for round := 1; round <= 3; round++ {
		hash := h.user(h.desk).PhoneGateway.Hash
		if key, _ := h.gateway.lookup(hash); key.Disabled != (round > 1) {
			t.Fatalf("round %d: the phone key's disabled flag is %v", round, key.Disabled)
		}
		if round == 1 {
			if err := h.gateway.spend(hash, 5.00); err != nil {
				t.Fatal(err)
			}
		}
		phone = h.pairPhone("Replacement iPhone", "")
		grant := h.mustGrant(phone)
		if grant["status"] != "exhausted" || grant["used_units"].(float64) != 500 || grant["remaining_units"].(float64) != 0 {
			t.Fatalf("round %d: a spent allowance came back after pairing again: %v", round, grant)
		}
	}
	// The whole allowance went through the phone, so the desk key has no
	// room left and its cap stops at zero rather than going below it.
	if key, _ := h.gateway.lookup(u.Gateway.Hash); !key.Disabled || key.LimitUSD != 0 {
		t.Fatalf("the desk key after the phone spent everything: %+v", key)
	}
}

func TestAHangingGatewayStillForgetsThePhoneKey(t *testing.T) {
	h := newSyncHarness(t)
	h.server.gatewayPatience = 50 * time.Millisecond
	counting := h.countGatewayCalls()
	first := grantedKey(h.mustGrant(h.phone))

	counting.mu.Lock()
	counting.hang = true
	counting.mu.Unlock()
	next := h.pairPhone("Replacement iPhone", "")
	if u := h.user(h.desk); u.PhoneGateway != (gatewayKey{}) {
		t.Fatalf("the old phone's key must be forgotten even when the gateway never answers: %+v", u.PhoneGateway)
	}
	counting.mu.Lock()
	counting.hang = false
	counting.mu.Unlock()
	if grantedKey(h.mustGrant(next)) == first {
		t.Fatal("the next phone must not inherit the old phone's key")
	}
}

func (h *syncHarness) phoneDeviceID() string {
	h.t.Helper()
	phone, err := h.server.store.phoneDevice(context.Background(), h.vault)
	if err != nil {
		h.t.Fatalf("phone device: %v", err)
	}
	return phone.ID
}

func TestThePhoneKeyIsRevokedWithThePhone(t *testing.T) {
	lapse := func(h *syncHarness, lapsed bool) {
		h.t.Helper()
		status, _ := h.call("POST", "/admin/users/"+h.user(h.desk).ID+"/vault/lapse", "admin-secret", map[string]any{"lapsed": lapsed})
		if status != http.StatusNoContent {
			h.t.Fatalf("lapse %v: %d", lapsed, status)
		}
	}
	cases := []struct {
		name string
		end  func(h *syncHarness)
		// Returns the credential of a phone that may ask again, or "" when
		// nothing can.
		resume func(h *syncHarness) string
	}{
		{
			name: "device revoke",
			end: func(h *syncHarness) {
				if status, _ := h.call("POST", "/v1/vault/devices/"+h.phoneDeviceID()+"/revoke", h.desk, nil); status != http.StatusNoContent {
					h.t.Fatalf("revoke phone: %d", status)
				}
			},
			resume: func(h *syncHarness) string { return h.pairPhone("Next iPhone", "") },
		},
		{
			name:   "second pairing",
			end:    func(h *syncHarness) { h.phone = h.pairPhone("Replacement iPhone", "") },
			resume: func(h *syncHarness) string { return h.phone },
		},
		{
			name: "vault delete",
			end: func(h *syncHarness) {
				if status, _ := h.call("DELETE", "/v1/vault", h.desk, nil); status != http.StatusNoContent {
					h.t.Fatalf("delete vault: %d", status)
				}
			},
			resume: func(h *syncHarness) string {
				if status, body := h.call("POST", "/v1/vault", h.desk, map[string]any{"device_name": "Test Mac", "key_check": testKeyCheck}); status != http.StatusOK {
					h.t.Fatalf("recreate vault: %d %v", status, body)
				}
				return h.pairPhone("Next iPhone", "")
			},
		},
		{
			name: "vault reset",
			end: func(h *syncHarness) {
				status, body := h.call("POST", "/v1/vault/reset", h.desk, map[string]any{"key_check": "fedcba9876543210fedcba9876543210"})
				if status != http.StatusOK {
					h.t.Fatalf("reset: %d %v", status, body)
				}
			},
			resume: func(h *syncHarness) string { return h.pairPhone("Next iPhone", "") },
		},
		{
			name: "lapse",
			end:  func(h *syncHarness) { lapse(h, true) },
			resume: func(h *syncHarness) string {
				lapse(h, false)
				return h.phone
			},
		},
		{
			name: "user revoke",
			end: func(h *syncHarness) {
				if status, _ := h.call("POST", "/v1/disconnect", h.desk, nil); status != http.StatusNoContent {
					h.t.Fatalf("disconnect: %d", status)
				}
			},
			resume: func(h *syncHarness) string { return "" },
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			h := newSyncHarness(t)
			oldPhone := h.phone
			first := grantedKey(h.mustGrant(h.phone))
			hash := h.user(h.desk).PhoneGateway.Hash

			test.end(h)

			if key, _ := h.gateway.lookup(hash); !key.Revoked {
				t.Fatalf("the phone key should be deleted at the gateway: %+v", key)
			}
			if u := h.user(h.desk); u.PhoneGateway != (gatewayKey{}) || u.PhoneUsageBaselineUSD != 0 {
				t.Fatalf("the phone key should be forgotten: %+v", u.PhoneGateway)
			}
			if status, body := h.grant(oldPhone); status == http.StatusOK {
				t.Fatalf("the old phone should get no grant: %d %v", status, body)
			}

			next := test.resume(h)
			if next == "" {
				if !h.gateway.mintedAndRevoked(2, 2) {
					t.Fatal("revoking the user should delete both keys and mint nothing")
				}
				return
			}
			second := grantedKey(h.mustGrant(next))
			if second == "" || second == first {
				t.Fatalf("the next grant should mint a fresh key, got %q after %q", second, first)
			}
			if !h.gateway.mintedAndRevoked(3, 1) {
				t.Fatal("one phone key revoked, one minted in its place")
			}
		})
	}
}

func TestAGatewayFailureStillForgetsThePhoneKey(t *testing.T) {
	h := newSyncHarness(t)
	counting := h.countGatewayCalls()
	first := grantedKey(h.mustGrant(h.phone))

	// Disconnecting the phone can't be undone, so the key is forgotten even
	// though the gateway wouldn't delete it.
	counting.failRevoke = true
	if status, _ := h.call("POST", "/v1/vault/devices/"+h.phoneDeviceID()+"/revoke", h.desk, nil); status != http.StatusNoContent {
		t.Fatalf("revoke phone: %d", status)
	}
	if u := h.user(h.desk); u.PhoneGateway != (gatewayKey{}) {
		t.Fatalf("the secret must not wait for the next phone: %+v", u.PhoneGateway)
	}
	next := h.pairPhone("Next iPhone", "")
	second := grantedKey(h.mustGrant(next))
	if second == first {
		t.Fatal("the next phone must not inherit the old phone's key")
	}

	// Revoking the user can be retried, so there the failure is reported
	// and nothing is forgotten until the gateway agrees.
	status, body := h.call("POST", "/v1/disconnect", h.desk, nil)
	if status != http.StatusInternalServerError || body["error"] == "" {
		t.Fatalf("disconnect with the gateway down: %d %v", status, body)
	}
	if u := h.user(h.desk); u.Status != userActive || u.PhoneGateway.Secret != second {
		t.Fatalf("a failed revocation leaves the user as they were: %+v", u)
	}
	counting.failRevoke = false
	if status, _ := h.call("POST", "/v1/disconnect", h.desk, nil); status != http.StatusNoContent {
		t.Fatalf("disconnect once the gateway is back: %d", status)
	}
	u := h.user(h.desk)
	if u.Status != userRevoked || u.PhoneGateway != (gatewayKey{}) || u.Gateway.Secret != "" {
		t.Fatalf("after the retry: %+v", u)
	}
}

func TestAUserWithoutAPhoneKeyCostsNoExtraGatewayCalls(t *testing.T) {
	h := newHarness(t)
	counting := h.countGatewayCalls()
	expect := func(step string, mint, usage, configure, revoke int) {
		t.Helper()
		got := []int{counting.counted("mint"), counting.counted("usage"), counting.counted("configure"), counting.counted("revoke")}
		want := []int{mint, usage, configure, revoke}
		for i := range want {
			if got[i] != want[i] {
				t.Fatalf("%s: gateway calls (mint, usage, configure, revoke) = %v, want %v", step, got, want)
			}
		}
	}

	credential, _ := h.connect(h.invite())
	hash := h.keyHash(credential)
	userID := h.user(credential).ID
	expect("connect", 1, 0, 0, 0)

	h.entitlement(credential)
	expect("a read inside the sync interval", 1, 0, 0, 0)

	if err := h.gateway.spend(hash, 2); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	h.entitlement(credential)
	expect("a stale read", 1, 1, 0, 0)

	if err := h.gateway.spend(hash, 3); err != nil {
		t.Fatal(err)
	}
	h.clock = h.clock.Add(time.Minute)
	h.entitlement(credential)
	expect("exhaustion", 1, 2, 1, 0)

	h.call("POST", "/admin/users/"+userID+"/allowance", "admin-secret", map[string]any{"adjust_units": 200})
	expect("a top-up", 1, 3, 2, 0)

	h.call("POST", "/admin/users/"+userID+"/allowance", "admin-secret", map[string]any{"reset": true})
	expect("an admin reset", 1, 5, 3, 0)

	h.clock = h.clock.Add(31 * 24 * time.Hour)
	h.entitlement(credential)
	expect("a rollover", 1, 6, 4, 0)

	if status, _ := h.call("POST", "/v1/disconnect", credential, nil); status != http.StatusNoContent {
		t.Fatalf("disconnect: %d", status)
	}
	expect("a revocation", 1, 6, 4, 1)
	if u := h.user(credential); u.PhoneGateway != (gatewayKey{}) || u.PhoneUsageBaselineUSD != 0 {
		t.Fatalf("no phone key should ever have appeared: %+v", u.PhoneGateway)
	}
}
