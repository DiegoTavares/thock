package main

import (
	"context"
	"encoding/base64"
	"fmt"
	"net/http"
	"os"
	"regexp"
	"sort"
	"strings"
	"testing"
	"time"
)

// Security properties of the HTTP surface. Each test pins something an
// attacker would otherwise be able to do; known gaps are recorded as skipped
// tests that state the behaviour we want.

type routeAuth int

const (
	authPublic routeAuth = iota
	authUser
	authVault
	authAdmin
	authBlobToken
)

// Every route registered in routes(), with the credential it demands.
// TestEveryRouteIsClassified fails when a route is added without an entry
// here, so a new handler can't ship without an auth decision.
var routeTable = map[string]routeAuth{
	"POST /v1/connect":                          authPublic,
	"GET /v1/entitlement":                       authUser,
	"POST /v1/disconnect":                       authUser,
	"POST /v1/vault":                            authVault,
	"GET /v1/vault":                             authVault,
	"DELETE /v1/vault":                          authVault,
	"POST /v1/vault/reset":                      authVault,
	"POST /v1/vault/pairings":                   authVault,
	"POST /v1/vault/pair":                       authPublic,
	"GET /v1/vault/devices":                     authVault,
	"PATCH /v1/vault/devices/me":                authVault,
	"POST /v1/vault/devices/{device_id}/revoke": authVault,
	"GET /v1/vault/files":                       authVault,
	"POST /v1/vault/files/{path...}":            authVault,
	"DELETE /v1/vault/files/{path...}":          authVault,
	"POST /v1/vault/writes":                     authVault,
	"GET /v1/vault/writes":                      authVault,
	"POST /v1/vault/writes/ack":                 authVault,
	"GET /v1/vault/feed":                        authVault,
	"GET /v1/vault/agent":                       authVault,
	"POST /v1/vault/feedback":                   authVault,
	"PUT /v1/vault/blobs/{token}":               authBlobToken,
	"GET /v1/vault/blobs/{token}":               authBlobToken,
	"GET /admin/plans":                          authAdmin,
	"PUT /admin/plans/{id}":                     authAdmin,
	"PUT /admin/settings":                       authAdmin,
	"POST /admin/invites":                       authAdmin,
	"GET /admin/invites":                        authAdmin,
	"GET /admin/users":                          authAdmin,
	"POST /admin/users/{id}/allowance":          authAdmin,
	"POST /admin/users/{id}/revoke":             authAdmin,
	"GET /admin/users/{id}/ledger":              authAdmin,
	"POST /admin/users/{id}/vault/lapse":        authAdmin,
	"GET /health":                               authPublic,
	"GET /healthz":                              authPublic,
	"/":                                         authPublic,
}

func TestEveryRouteIsClassified(t *testing.T) {
	source, err := os.ReadFile("main.go")
	if err != nil {
		t.Fatal(err)
	}
	registered := regexp.MustCompile(`mux\.HandleFunc\("([^"]+)"`).FindAllStringSubmatch(string(source), -1)
	if len(registered) == 0 {
		t.Fatal("found no routes in main.go; the pattern above is out of date")
	}
	seen := map[string]bool{}
	for _, match := range registered {
		pattern := match[1]
		seen[pattern] = true
		if _, ok := routeTable[pattern]; !ok {
			t.Errorf("route %q has no entry in routeTable", pattern)
		}
	}
	for pattern := range routeTable {
		if !seen[pattern] {
			t.Errorf("routeTable lists %q, which main.go no longer registers", pattern)
		}
	}
}

// concretePath fills a route pattern's wildcards with plausible values.
func concretePath(pattern string) (method, path string) {
	method, path, _ = strings.Cut(pattern, " ")
	replacer := strings.NewReplacer(
		"{device_id}", "0123456789abcdef",
		"{path...}", "daily/2026-10-02.md",
		"{id}", "someone",
		"{token}", "x.y",
	)
	return method, replacer.Replace(path)
}

func protectedRoutes(kinds ...routeAuth) []string {
	var patterns []string
	for pattern, kind := range routeTable {
		for _, want := range kinds {
			if kind == want {
				patterns = append(patterns, pattern)
			}
		}
	}
	sort.Strings(patterns)
	return patterns
}

func TestProtectedRoutesRefuseMissingAndForgedCredentials(t *testing.T) {
	h := newSyncHarness(t)
	forged := map[string]string{
		"none":                  "",
		"unknown desk":          "tpk_" + strings.Repeat("0", 48),
		"unknown phone":         "tpp_" + strings.Repeat("0", 48),
		"garbage":               "not-a-credential",
		"desk credential hash":  hashCredential(h.desk),
		"phone credential hash": "tpp_" + strings.TrimPrefix(hashCredential(h.phone), "tpp_"),
	}
	for _, pattern := range protectedRoutes(authUser, authVault) {
		method, path := concretePath(pattern)
		for name, token := range forged {
			status, body := h.call(method, path, token, map[string]any{})
			if status != http.StatusUnauthorized || body["code"] != "unauthorized" {
				t.Errorf("%s with %s credential: %d %v", pattern, name, status, body)
			}
		}
		// A non-bearer scheme carrying a real credential is not a credential.
		response, _ := h.raw(method, h.http.URL+path, "", nil, "")
		if response.StatusCode != http.StatusUnauthorized {
			t.Errorf("%s without Authorization: %d", pattern, response.StatusCode)
		}
		request, err := http.NewRequest(method, h.http.URL+path, nil)
		if err != nil {
			t.Fatal(err)
		}
		request.Header.Set("Authorization", "Basic "+h.desk)
		basic, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Fatal(err)
		}
		basic.Body.Close()
		if basic.StatusCode != http.StatusUnauthorized {
			t.Errorf("%s with a Basic header: %d", pattern, basic.StatusCode)
		}
	}
}

func TestAdminRoutesRefuseEverythingButTheAdminToken(t *testing.T) {
	h := newSyncHarness(t)
	tokens := map[string]string{
		"none":             "",
		"wrong":            "wrong",
		"prefix":           "admin-secre",
		"extended":         "admin-secret-and-more",
		"other case":       "ADMIN-SECRET",
		"desk credential":  h.desk,
		"phone credential": h.phone,
	}
	for _, pattern := range protectedRoutes(authAdmin) {
		method, path := concretePath(pattern)
		for name, token := range tokens {
			status, body := h.call(method, path, token, map[string]any{})
			if status != http.StatusUnauthorized {
				t.Errorf("%s with %s token: %d %v", pattern, name, status, body)
			}
		}
	}
}

func TestThePhoneCredentialNeverReachesThePlusAccount(t *testing.T) {
	h := newSyncHarness(t)
	// The entitlement carries the OpenRouter key; a phone must not see it,
	// and must not be able to disconnect the desk's account.
	for _, pattern := range protectedRoutes(authUser) {
		method, path := concretePath(pattern)
		status, body := h.call(method, path, h.phone, nil)
		if status != http.StatusUnauthorized {
			t.Errorf("%s with the phone credential: %d %v", pattern, status, body)
		}
	}
	if status, _ := h.entitlement(h.desk); status != http.StatusOK {
		t.Fatalf("the desk should still be connected: %d", status)
	}
}

func TestRolesAreEnforcedOnEveryVaultRoute(t *testing.T) {
	h := newSyncHarness(t)
	deskOnlyRoutes := []string{
		"POST /v1/vault", "DELETE /v1/vault", "POST /v1/vault/reset", "POST /v1/vault/pairings",
		"POST /v1/vault/devices/{device_id}/revoke", "POST /v1/vault/files/{path...}",
		"DELETE /v1/vault/files/{path...}", "GET /v1/vault/writes", "POST /v1/vault/writes/ack",
	}
	phoneOnlyRoutes := []string{"POST /v1/vault/writes", "PATCH /v1/vault/devices/me", "GET /v1/vault/agent", "POST /v1/vault/feedback"}
	check := func(patterns []string, token string) {
		for _, pattern := range patterns {
			method, path := concretePath(pattern)
			status, body := h.call(method, path, token, map[string]any{})
			if status != http.StatusForbidden || body["code"] != "role_forbidden" {
				t.Errorf("%s with the wrong role: %d %v", pattern, status, body)
			}
		}
	}
	check(deskOnlyRoutes, h.phone)
	check(phoneOnlyRoutes, h.desk)
	if status, body := h.call("GET", "/v1/vault", h.phone, nil); status != http.StatusOK {
		t.Fatalf("the phone should still work after its refusals: %d %v", status, body)
	}
}

// newTenant connects another user with their own vault and phone on the
// same server.
func (h *syncHarness) newTenant() *syncHarness {
	h.t.Helper()
	desk, _ := h.connect(h.invite())
	status, body := h.call("POST", "/v1/vault", desk, map[string]any{"device_name": "Other Mac", "key_check": testKeyCheck})
	if status != http.StatusOK {
		h.t.Fatalf("other vault: %d %v", status, body)
	}
	other := &syncHarness{harness: h.harness, desk: desk, vault: body["vault_id"].(string)}
	other.phone = other.pairPhone("Other iPhone", "bb22")
	return other
}

func TestOneVaultCannotReachAnother(t *testing.T) {
	victim := newSyncHarness(t)
	attacker := victim.newTenant()
	if attacker.vault == victim.vault {
		t.Fatal("two users share a vault")
	}

	victim.mustUpload("private/diary.md", 0, "victim secret")
	if status, body := victim.write("11111111-2222-3333-4444-555555555555", "private/diary.md", 1, "victim write"); status != http.StatusCreated {
		t.Fatalf("victim write: %d %v", status, body)
	}
	// A pending upload the attacker will try to commit into their own vault.
	pendingID, pendingHash, _ := blobOf("victim pending")
	if status, body := victim.call("POST", "/v1/vault/files/private/pending.md", victim.desk, map[string]any{
		"expected_version": 0, "blob_id": pendingID, "size_bytes": 18, "content_hash": pendingHash,
	}); status != http.StatusOK {
		t.Fatalf("victim begin: %d %v", status, body)
	}

	for _, token := range []string{attacker.desk, attacker.phone} {
		listing := attacker.files(token, "")
		if files := listing["files"].([]any); len(files) != 0 {
			t.Errorf("the attacker sees the victim's files: %v", files)
		}
		status, body := attacker.call("GET", "/v1/vault/devices", token, nil)
		if status != http.StatusOK {
			t.Fatalf("devices: %d %v", status, body)
		}
		for _, d := range body["devices"].([]any) {
			if name := d.(map[string]any)["name"]; name == "Test Mac" || name == "Test iPhone" {
				t.Errorf("the attacker sees the victim's device %v", d)
			}
		}
	}
	status, body := attacker.call("GET", "/v1/vault/writes?after=0", attacker.desk, nil)
	if status != http.StatusOK || len(body["writes"].([]any)) != 0 {
		t.Errorf("the attacker reads the victim's writes: %d %v", status, body)
	}
	status, body = attacker.call("POST", "/v1/vault/writes/ack", attacker.desk, map[string]any{"through_seq": 1})
	if status != http.StatusBadRequest {
		t.Errorf("the attacker acks the victim's writes: %d %v", status, body)
	}

	// Commit the victim's pending blob id, under the victim's path and under
	// one of the attacker's own.
	for _, path := range []string{"private/pending.md", "stolen.md"} {
		status, body = attacker.call("POST", "/v1/vault/files/"+path+"/commit", attacker.desk, map[string]any{"expected_version": 0, "blob_id": pendingID})
		if status != http.StatusUnprocessableEntity || body["code"] != "blob_missing" {
			t.Errorf("the attacker commits the victim's blob as %s: %d %v", path, status, body)
		}
	}
	// Overwrite or delete the victim's file by path.
	status, body = attacker.call("DELETE", "/v1/vault/files/private/diary.md", attacker.desk, map[string]any{"expected_version": 1})
	if status != http.StatusConflict && status != http.StatusNotFound {
		t.Errorf("the attacker deletes by the victim's path: %d %v", status, body)
	}

	// Disconnect the victim's phone by its device id.
	status, body = victim.call("GET", "/v1/vault/devices", victim.desk, nil)
	if status != http.StatusOK {
		t.Fatalf("victim devices: %d %v", status, body)
	}
	var victimPhoneID string
	for _, d := range body["devices"].([]any) {
		if d.(map[string]any)["role"] == "phone" {
			victimPhoneID = d.(map[string]any)["device_id"].(string)
		}
	}
	status, body = attacker.call("POST", "/v1/vault/devices/"+victimPhoneID+"/revoke", attacker.desk, nil)
	if status != http.StatusNotFound {
		t.Errorf("the attacker revokes the victim's phone: %d %v", status, body)
	}

	// Nothing above touched the victim.
	listing := victim.files(victim.phone, "")
	if files := listing["files"].([]any); len(files) != 1 || files[0].(map[string]any)["path"] != "private/diary.md" {
		t.Errorf("the victim's files changed: %v", files)
	}
	status, body = victim.call("GET", "/v1/vault/writes?after=0", victim.desk, nil)
	if status != http.StatusOK || len(body["writes"].([]any)) != 1 {
		t.Errorf("the victim's writes changed: %d %v", status, body)
	}
	if status, body := victim.call("GET", "/v1/vault", victim.phone, nil); status != http.StatusOK {
		t.Errorf("the victim's phone was disconnected: %d %v", status, body)
	}
}

func TestTheFeedOnlyCarriesTheOwnVault(t *testing.T) {
	victim := newSyncHarness(t)
	attacker := victim.newTenant()
	attackerEvents, closeAttacker := attacker.feed(attacker.phone)
	defer closeAttacker()
	victimEvents, closeVictim := victim.feed(victim.phone)
	defer closeVictim()

	victim.mustUpload("daily/today.md", 0, "victim")
	expectEvent(t, victimEvents, "file:")
	expectNoEvent(t, attackerEvents)
}

func TestBlobLinksAreBoundToTheirVaultBlobAndMethod(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("notes/a.md", 0, "alpha")
	listing := h.files(h.phone, "")
	downloadURL := listing["files"].([]any)[0].(map[string]any)["download_url"].(string)
	base, token, ok := strings.Cut(downloadURL, "/v1/vault/blobs/")
	if !ok {
		t.Fatalf("unexpected download url %q", downloadURL)
	}
	body, signature, ok := strings.Cut(token, ".")
	if !ok {
		t.Fatalf("unexpected token %q", token)
	}

	raw, err := base64.RawURLEncoding.DecodeString(body)
	if err != nil {
		t.Fatal(err)
	}
	other := victimToken(t, raw, h.vault)
	tampered := base + "/v1/vault/blobs/" + base64.RawURLEncoding.EncodeToString(other) + "." + signature
	if response, _ := h.raw("GET", tampered, "", nil, ""); response.StatusCode != http.StatusUnauthorized {
		t.Errorf("a token re-pointed at another vault: %d", response.StatusCode)
	}
	if response, _ := h.raw("GET", downloadURL+"x", "", nil, ""); response.StatusCode != http.StatusUnauthorized {
		t.Errorf("a token with a broken signature: %d", response.StatusCode)
	}
	if response, _ := h.raw("GET", base+"/v1/vault/blobs/"+body, "", nil, ""); response.StatusCode != http.StatusUnauthorized {
		t.Errorf("an unsigned token: %d", response.StatusCode)
	}
	// A download link can't be used to upload.
	if response, _ := h.raw("PUT", downloadURL, "", []byte("TVS1alpha"), "application/octet-stream"); response.StatusCode != http.StatusMethodNotAllowed {
		t.Errorf("PUT through a download link: %d", response.StatusCode)
	}
	// Links die at their expiry.
	h.clock = h.clock.Add(signedURLLifetime + time.Second)
	if response, _ := h.raw("GET", downloadURL, "", nil, ""); response.StatusCode != http.StatusGone {
		t.Errorf("an expired link: %d", response.StatusCode)
	}
}

// victimToken rewrites the vault id in a token body, keeping it valid JSON.
func victimToken(t *testing.T, raw []byte, vaultID string) []byte {
	t.Helper()
	rewritten := strings.Replace(string(raw), `"v":"`+vaultID+`"`, `"v":"ffffffffffffffff"`, 1)
	if rewritten == string(raw) {
		t.Fatalf("token %s doesn't name vault %s", raw, vaultID)
	}
	return []byte(rewritten)
}

func TestUploadLinksAcceptOnlyTheAnnouncedSize(t *testing.T) {
	h := newSyncHarness(t)
	blobID, hash, data := blobOf("sized")
	status, body := h.call("POST", "/v1/vault/files/sized.md", h.desk, map[string]any{
		"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash,
	})
	if status != http.StatusOK {
		t.Fatalf("begin: %d %v", status, body)
	}
	url := body["upload"].(map[string]any)["url"].(string)
	larger := append(append([]byte{}, data...), make([]byte, 1<<20)...)
	if response, _ := h.raw("PUT", url, "", larger, "application/octet-stream"); response.StatusCode != http.StatusBadRequest {
		t.Errorf("a larger body than announced: %d", response.StatusCode)
	}
	if response, _ := h.raw("PUT", url, "", data[:len(data)-1], "application/octet-stream"); response.StatusCode != http.StatusBadRequest {
		t.Errorf("a shorter body than announced: %d", response.StatusCode)
	}
	if size, err := h.server.blobs.stat(context.Background(), h.vault, blobID); err == nil {
		t.Errorf("a refused upload left a %d byte object behind", size)
	}
}

func TestOversizedRequestsAreRefused(t *testing.T) {
	h := newSyncHarness(t)
	padding := strings.Repeat("x", 1<<17)

	status, body := h.call("POST", "/v1/connect", "", map[string]any{"invite_code": "THOCK-AAAA-AAAA", "device": padding})
	if status != http.StatusBadRequest {
		t.Errorf("an oversized connect: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": "AAAA-AAAA", "device_name": padding})
	if status != http.StatusBadRequest {
		t.Errorf("an oversized pair: %d %v", status, body)
	}
	status, body = h.call("PATCH", "/v1/vault/devices/me", h.phone, map[string]any{"device_name": padding})
	if status != http.StatusBadRequest {
		t.Errorf("an oversized device patch: %d %v", status, body)
	}
	status, body = h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "test", "note": padding})
	if status != http.StatusBadRequest {
		t.Errorf("an oversized admin body: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/writes", h.phone, map[string]any{
		"client_id": "11111111-2222-3333-4444-555555555555", "path": "big.md", "base_version": 0,
		"payload": strings.Repeat("A", maxWriteBodyBytes),
	})
	if status != http.StatusRequestEntityTooLarge {
		t.Errorf("an oversized write: %d %v", status, body)
	}
	blobID, hash, _ := blobOf("big")
	status, body = h.call("POST", "/v1/vault/files/big.md", h.desk, map[string]any{
		"expected_version": 0, "blob_id": blobID, "size_bytes": maxBlobBytes + 1, "content_hash": hash,
	})
	if status != http.StatusRequestEntityTooLarge {
		t.Errorf("an oversized upload announcement: %d %v", status, body)
	}
}

func TestHostilePathsNeverReachTheStore(t *testing.T) {
	h := newSyncHarness(t)
	blobID, hash, data := blobOf("hostile")
	begin := map[string]any{"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash}
	hostile := []string{
		"..%2F..%2Fetc%2Fpasswd.md",
		"a%2F..%2F..%2Fb.md",
		"%2E%2E/x.md",
		"a/%2e%2e/b.md",
		"%2Fabsolute.md",
		".git%2Fconfig.md",
		".thock/history/x.md",
		"x.md%2Fcommit%2Fcommit",
		"run.sh",
		"x.md.exe",
	}
	for _, path := range hostile {
		status, body := h.call("POST", "/v1/vault/files/"+path, h.desk, begin)
		if status == http.StatusOK {
			t.Errorf("begin accepted %q: %v", path, body)
		}
		status, body = h.call("DELETE", "/v1/vault/files/"+path, h.desk, map[string]any{"expected_version": 1})
		if status == http.StatusOK {
			t.Errorf("delete accepted %q: %v", path, body)
		}
	}
	for _, path := range []string{"../escape.md", "/abs.md", "a//b.md", ".git/x.md", "x.exe"} {
		status, body := h.write("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", path, 0, "payload")
		if status != http.StatusUnprocessableEntity || body["code"] != "path_not_allowed" {
			t.Errorf("phone write to %q: %d %v", path, status, body)
		}
	}
	listing := h.files(h.phone, "?since=0")
	if files := listing["files"].([]any); len(files) != 0 {
		t.Errorf("hostile paths reached the store: %v", files)
	}
	status, body := h.call("GET", "/v1/vault/writes?after=0", h.desk, nil)
	if status != http.StatusOK || len(body["writes"].([]any)) != 0 {
		t.Errorf("hostile writes were queued: %d %v", status, body)
	}
}

func TestSecretsAreStoredOnlyAsHashes(t *testing.T) {
	h := newSyncHarness(t)
	ctx := context.Background()
	pool := h.server.store.pool

	var deskHash string
	if err := pool.QueryRow(ctx, "select credential_hash from users where credential_hash = $1", hashCredential(h.desk)).Scan(&deskHash); err != nil {
		t.Fatalf("desk credential hash: %v", err)
	}
	var phoneHash string
	if err := pool.QueryRow(ctx, "select credential_hash from devices where role = 'phone'").Scan(&phoneHash); err != nil {
		t.Fatalf("phone credential hash: %v", err)
	}
	if phoneHash != hashCredential(h.phone) {
		t.Errorf("the phone credential is stored as %q", phoneHash)
	}

	status, body := h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != http.StatusCreated {
		t.Fatalf("pairing: %d %v", status, body)
	}
	code := body["code"].(string)
	var pairingHash string
	if err := pool.QueryRow(ctx, "select code_hash from pairings where used_at is null").Scan(&pairingHash); err != nil {
		t.Fatalf("pairing hash: %v", err)
	}
	if pairingHash != hashCredential(normalizePairingCode(code)) {
		t.Errorf("the pairing code is stored as %q", pairingHash)
	}

	// No column anywhere holds a raw credential or code.
	for _, secret := range []string{h.desk, h.phone, code, normalizePairingCode(code)} {
		var hits int
		err := pool.QueryRow(ctx, `select
			(select count(*) from users where credential_hash = $1 or device = $1) +
			(select count(*) from devices where credential_hash = $1 or name = $1) +
			(select count(*) from pairings where code_hash = $1)`, secret).Scan(&hits)
		if err != nil {
			t.Fatal(err)
		}
		if hits != 0 {
			t.Errorf("a raw secret is stored in the clear")
		}
	}
}

func TestPairingCodesAreSingleUseAndOnlyTheNewestWorks(t *testing.T) {
	h := newSyncHarness(t)
	status, body := h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != http.StatusCreated {
		t.Fatalf("first pairing: %d %v", status, body)
	}
	first := body["code"].(string)
	status, body = h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != http.StatusCreated {
		t.Fatalf("second pairing: %d %v", status, body)
	}
	second := body["code"].(string)
	if first == second {
		t.Fatal("two pairings minted the same code")
	}
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": first})
	if status != http.StatusNotFound || body["code"] != "pairing_invalid" {
		t.Errorf("a superseded code: %d %v", status, body)
	}
	if status, body := h.call("GET", "/v1/vault", h.phone, nil); status != http.StatusOK {
		t.Errorf("a failed pairing disconnected the phone: %d %v", status, body)
	}

	for _, malformed := range []string{"", "AAAA", "AAAA-AAAA-AAAA", strings.Repeat("A", 1000), "'; drop table pairings; --"} {
		status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": malformed})
		if status != http.StatusNotFound || body["code"] != "pairing_invalid" {
			t.Errorf("malformed code %q: %d %v", malformed, status, body)
		}
	}
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": second})
	if status != http.StatusCreated {
		t.Fatalf("the newest code: %d %v", status, body)
	}
}

func TestPairingCodesUseTheirWholeAlphabet(t *testing.T) {
	// 32 symbols over a byte keeps the draw unbiased; a 31 or 33 symbol
	// alphabet would quietly cost entropy.
	if 256%len(pairingAlphabet) != 0 {
		t.Fatalf("the pairing alphabet has %d symbols, which biases the code", len(pairingAlphabet))
	}
	seen := map[string]bool{}
	for range 2000 {
		code, err := pairingCode()
		if err != nil {
			t.Fatal(err)
		}
		if seen[code] {
			t.Fatalf("pairing code %q repeated within 2000 draws", code)
		}
		seen[code] = true
		if len(code) != 9 || code[4] != '-' || strings.ContainsAny(code, "IO01") {
			t.Fatalf("pairing code shape: %q", code)
		}
	}
}

func TestRevokedAccountsLoseWriteAccessEverywhere(t *testing.T) {
	h := newSyncHarness(t)
	status, _ := h.call("POST", "/v1/disconnect", h.desk, nil)
	if status != http.StatusNoContent {
		t.Fatalf("disconnect: %d", status)
	}
	for _, pattern := range protectedRoutes(authUser, authVault) {
		// Deleting the vault copy is the one thing a disconnected desk may
		// still do; TestADisconnectedDeskCanDeleteItsVaultCopy covers it.
		if pattern == "DELETE /v1/vault" {
			continue
		}
		method, path := concretePath(pattern)
		status, body := h.call(method, path, h.desk, map[string]any{})
		if status != http.StatusForbidden || body["code"] != "revoked" {
			t.Errorf("%s with a revoked desk: %d %v", pattern, status, body)
		}
	}
	status, body := h.write("99999999-2222-3333-4444-555555555555", "x.md", 0, "after revoke")
	if status != http.StatusForbidden || body["code"] != "plus_lapsed" {
		t.Errorf("a phone write after revocation: %d %v", status, body)
	}
	status, body = h.call("PATCH", "/v1/vault/devices/me", h.phone, map[string]any{"device_name": "x"})
	if status != http.StatusForbidden || body["code"] != "plus_lapsed" {
		t.Errorf("a phone patch after revocation: %d %v", status, body)
	}
}

func TestErrorsDontLeakInternals(t *testing.T) {
	h := newSyncHarness(t)
	probes := []struct {
		method, path, token string
		body                any
	}{
		{"POST", "/v1/connect", "", "not an object"},
		{"POST", "/v1/connect", "", map[string]any{"invite_code": "THOCK-NOPE-NOPE"}},
		{"POST", "/v1/vault/pair", "", map[string]any{"code": "ZZZZ-ZZZZ"}},
		{"GET", "/v1/vault/files?since=abc", h.desk, nil},
		{"GET", "/v1/vault/files?limit=-1", h.desk, nil},
		{"POST", "/v1/vault/writes", h.phone, map[string]any{"client_id": "nope"}},
		{"GET", "/does/not/exist", "", nil},
	}
	for _, probe := range probes {
		status, body := h.call(probe.method, probe.path, probe.token, probe.body)
		if status < 400 {
			t.Errorf("%s %s: %d", probe.method, probe.path, status)
		}
		message, _ := body["error"].(string)
		for _, leak := range []string{"pgx", "postgres", "SQLSTATE", "sql:", "goroutine", ".go:", "runtime error"} {
			if strings.Contains(strings.ToLower(message), strings.ToLower(leak)) {
				t.Errorf("%s %s leaks %q: %q", probe.method, probe.path, leak, message)
			}
		}
	}
}

// --- known gaps: the behaviour we want, skipped until it holds ---

func TestKnownIssueUploadLinkOutlivesTheCommit(t *testing.T) {
	t.Skip("known issue: a signed upload URL stays valid for its whole 15 minutes after the commit, so the desk can overwrite a published blob (and, on GCS, at any size) without the server seeing it")
	h := newSyncHarness(t)
	blobID, hash, data := blobOf("original")
	status, body := h.call("POST", "/v1/vault/files/notes/a.md", h.desk, map[string]any{
		"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash,
	})
	if status != http.StatusOK {
		t.Fatalf("begin: %d %v", status, body)
	}
	url := body["upload"].(map[string]any)["url"].(string)
	if response, raw := h.raw("PUT", url, "", data, "application/octet-stream"); response.StatusCode != http.StatusOK {
		t.Fatalf("PUT: %d %s", response.StatusCode, raw)
	}
	if status, body := h.call("POST", "/v1/vault/files/notes/a.md/commit", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID}); status != http.StatusOK {
		t.Fatalf("commit: %d %v", status, body)
	}
	swapped := []byte(strings.Repeat("Z", len(data)))
	if response, _ := h.raw("PUT", url, "", swapped, "application/octet-stream"); response.StatusCode == http.StatusOK {
		t.Fatal("a committed blob was overwritten through its old upload link")
	}
}

func TestKnownIssueGCSUploadsAreNotSizeBound(t *testing.T) {
	t.Skip("known issue: GCS upload URLs don't bind the size (x-goog-content-length-range), so a PUT can store far more than size_bytes; only the commit-time stat notices, and only before the commit")
	store, _, signed := fakeGCSStore(map[string]int64{})
	upload, err := store.uploadURL(context.Background(), "vault", "blob", 100, time.Now().Add(signedURLLifetime))
	if err != nil {
		t.Fatal(err)
	}
	if upload.Headers["x-goog-content-length-range"] != "0,100" {
		t.Errorf("upload headers: %v", upload.Headers)
	}
	options := (*signed)[0]
	found := false
	for _, header := range options.Headers {
		found = found || strings.HasPrefix(strings.ToLower(header), "x-goog-content-length-range:")
	}
	if !found {
		t.Error("the signature doesn't cover a content-length range")
	}
}

func TestKnownIssueThePhoneWriteQueueIsUnbounded(t *testing.T) {
	t.Skip("known issue: unacked phone writes (up to 2 MB each) have no count or byte cap and don't count toward the vault quota, so one phone can fill the shared Postgres")
	h := newSyncHarness(t)
	payload := strings.Repeat("x", 1<<20)
	refused := false
	for i := range 400 {
		clientID := fmt.Sprintf("%08d-0000-0000-0000-000000000000", i)
		status, _ := h.write(clientID, "big.md", 0, payload)
		if status == http.StatusInsufficientStorage || status == http.StatusTooManyRequests {
			refused = true
			break
		}
	}
	if !refused {
		t.Fatal("400 MB of unacked writes were all accepted")
	}
}

func TestKnownIssueUnauthenticatedRoutesAreNotRateLimited(t *testing.T) {
	t.Skip("known issue: POST /v1/vault/pair and POST /v1/connect answer unlimited guesses; the contract reserves 429 slow_down but nothing emits it")
	h := newSyncHarness(t)
	limited := false
	for range 200 {
		status, _ := h.call("POST", "/v1/vault/pair", "", map[string]any{"code": "ZZZZ-ZZZZ"})
		if status == http.StatusTooManyRequests {
			limited = true
			break
		}
	}
	if !limited {
		t.Fatal("200 wrong pairing codes in a row were all answered")
	}
}

func TestKnownIssueControlCharactersInPaths(t *testing.T) {
	t.Skip("known issue: validateSyncPath accepts control characters (NUL reaches Postgres and answers 500; newlines, tabs and backslashes are stored and handed to the other device)")
	for _, path := range []string{"a\x00b.md", "line\nbreak.md", "tab\there.md", `..\..\escape.md`, "bell\x07.md"} {
		if err := validateSyncPath(path); err == nil {
			t.Errorf("%q should be refused", path)
		}
	}
}
