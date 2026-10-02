package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"
)

// The vault sync side of the contract (thock/specs/v34-vault-sync-api.md
// §12, backend list). A desk is a user credential; a phone is what pairing
// mints.

const testKeyCheck = "0123456789abcdef0123456789abcdef"

type syncHarness struct {
	*harness
	desk  string
	phone string
	vault string
}

func newSyncHarness(t *testing.T) *syncHarness {
	t.Helper()
	h := newHarness(t)
	desk, _ := h.connect(h.invite())
	sh := &syncHarness{harness: h, desk: desk}
	status, body := h.call("POST", "/v1/vault", desk, map[string]any{"device_name": "Test Mac", "key_check": testKeyCheck})
	if status != 200 {
		t.Fatalf("create vault: %d %v", status, body)
	}
	sh.vault = body["vault_id"].(string)
	sh.phone = sh.pairPhone("Test iPhone", "aa11")
	return sh
}

func (h *syncHarness) pairPhone(name, apnsToken string) string {
	h.t.Helper()
	status, body := h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != 201 {
		h.t.Fatalf("pairing: %d %v", status, body)
	}
	code := body["code"].(string)
	if len(code) != 9 || code[4] != '-' {
		h.t.Fatalf("pairing code shape: %q", code)
	}
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": strings.ToLower(code), "device_name": name, "platform": "ios", "apns_token": apnsToken})
	if status != 201 {
		h.t.Fatalf("pair: %d %v", status, body)
	}
	credential := body["credential"].(string)
	status, vault := h.call("GET", "/v1/vault", credential, nil)
	if status != 200 || vault["key_check"] != body["vault"].(map[string]any)["key_check"] {
		h.t.Fatalf("pair should hand the phone the vault's key check: %d %v", status, body)
	}
	return credential
}

func (h *syncHarness) raw(method, url, token string, body []byte, contentType string) (*http.Response, []byte) {
	h.t.Helper()
	request, err := http.NewRequest(method, url, bytes.NewReader(body))
	if err != nil {
		h.t.Fatal(err)
	}
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	if contentType != "" {
		request.Header.Set("Content-Type", contentType)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		h.t.Fatal(err)
	}
	defer response.Body.Close()
	raw, err := io.ReadAll(response.Body)
	if err != nil {
		h.t.Fatal(err)
	}
	return response, raw
}

func blobOf(content string) (blobID, hash string, data []byte) {
	data = []byte("TVS1" + content)
	sum := sha256.Sum256(data)
	idSum := sha256.Sum256([]byte("blob:" + content))
	return hex.EncodeToString(idSum[:16]), hex.EncodeToString(sum[:]), data
}

// upload runs begin → PUT → commit and returns the status and body of the
// step that ended it.
func (h *syncHarness) upload(path string, expected int64, content string) (int, map[string]any) {
	h.t.Helper()
	blobID, hash, data := blobOf(content)
	status, body := h.call("POST", "/v1/vault/files/"+path, h.desk, map[string]any{
		"expected_version": expected, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash,
	})
	if status != 200 {
		return status, body
	}
	upload := body["upload"].(map[string]any)
	response, raw := h.raw(upload["method"].(string), upload["url"].(string), "", data, "application/octet-stream")
	if response.StatusCode != 200 {
		h.t.Fatalf("PUT blob: %d %s", response.StatusCode, raw)
	}
	return h.call("POST", "/v1/vault/files/"+path+"/commit", h.desk, map[string]any{"expected_version": expected, "blob_id": blobID})
}

func (h *syncHarness) mustUpload(path string, expected int64, content string) int64 {
	h.t.Helper()
	status, body := h.upload(path, expected, content)
	if status != 200 {
		h.t.Fatalf("upload %s: %d %v", path, status, body)
	}
	return int64(body["version"].(float64))
}

func (h *syncHarness) files(token string, query string) map[string]any {
	h.t.Helper()
	status, body := h.call("GET", "/v1/vault/files"+query, token, nil)
	if status != 200 {
		h.t.Fatalf("files%s: %d %v", query, status, body)
	}
	return body
}

func (h *syncHarness) write(clientID, path string, base int64, payload string) (int, map[string]any) {
	h.t.Helper()
	return h.call("POST", "/v1/vault/writes", h.phone, map[string]any{
		"client_id": clientID, "path": path, "base_version": base, "payload": base64.StdEncoding.EncodeToString([]byte(payload)),
	})
}

// feed opens the SSE stream for token and returns a channel of
// "event:data" lines; pings are dropped.
func (h *syncHarness) feed(token string) (<-chan string, func()) {
	h.t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	request, err := http.NewRequestWithContext(ctx, "GET", h.http.URL+"/v1/vault/feed", nil)
	if err != nil {
		h.t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+token)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		h.t.Fatal(err)
	}
	if response.StatusCode != 200 || response.Header.Get("Content-Type") != "text/event-stream" {
		h.t.Fatalf("feed: %d %s", response.StatusCode, response.Header.Get("Content-Type"))
	}
	events := make(chan string, 16)
	go func() {
		defer response.Body.Close()
		scanner := bufio.NewScanner(response.Body)
		var kind string
		for scanner.Scan() {
			line := scanner.Text()
			switch {
			case strings.HasPrefix(line, "event: "):
				kind = strings.TrimPrefix(line, "event: ")
			case strings.HasPrefix(line, "data: "):
				events <- kind + ":" + strings.TrimPrefix(line, "data: ")
			}
		}
	}()
	// The ": connected" comment arrives once the subscription is live.
	time.Sleep(50 * time.Millisecond)
	return events, cancel
}

func expectEvent(t *testing.T, events <-chan string, prefix string) string {
	t.Helper()
	select {
	case event := <-events:
		if !strings.HasPrefix(event, prefix) {
			t.Fatalf("expected an event starting %q, got %q", prefix, event)
		}
		return event
	case <-time.After(3 * time.Second):
		t.Fatalf("no %q event arrived", prefix)
		return ""
	}
}

func expectNoEvent(t *testing.T, events <-chan string) {
	t.Helper()
	select {
	case event := <-events:
		t.Fatalf("unexpected event %q", event)
	case <-time.After(200 * time.Millisecond):
	}
}

func TestPairingHandsThePhoneACredentialOnce(t *testing.T) {
	h := newSyncHarness(t)

	status, body := h.call("GET", "/v1/vault", h.phone, nil)
	if status != 200 || body["status"] != "active" || body["quota_bytes"].(float64) != float64(200<<20) {
		t.Fatalf("vault from the phone: %d %v", status, body)
	}
	devices := body["devices"].([]any)
	if len(devices) != 2 {
		t.Fatalf("one desk and one phone expected: %v", devices)
	}
	status, body = h.call("GET", "/v1/entitlement", h.desk, nil)
	if status != 200 || body["vault"].(map[string]any)["quota_bytes"].(float64) != float64(200<<20) {
		t.Fatalf("entitlement should carry the vault: %d %v", status, body)
	}

	// A code is single use and unknown codes are refused.
	status, body = h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != 201 {
		t.Fatalf("pairing: %d %v", status, body)
	}
	code := body["code"].(string)
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": code})
	if status != 201 {
		t.Fatalf("pair: %d %v", status, body)
	}
	secondPhone := body["credential"].(string)
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": code})
	if status != 404 || body["code"] != "pairing_invalid" {
		t.Fatalf("reused code: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": "ZZZZ-ZZZZ"})
	if status != 404 || body["code"] != "pairing_invalid" {
		t.Fatalf("unknown code: %d %v", status, body)
	}

	// The second pairing replaced the first phone: the old credential is dead.
	status, body = h.call("GET", "/v1/vault", h.phone, nil)
	if status != 401 || body["code"] != "unauthorized" {
		t.Fatalf("replaced phone: %d %v", status, body)
	}

	// Codes expire after ten minutes.
	status, body = h.call("POST", "/v1/vault/pairings", h.desk, nil)
	if status != 201 {
		t.Fatalf("pairing: %d %v", status, body)
	}
	h.clock = h.clock.Add(11 * time.Minute)
	status, body = h.call("POST", "/v1/vault/pair", "", map[string]any{"code": body["code"]})
	if status != 410 || body["code"] != "pairing_expired" {
		t.Fatalf("expired code: %d %v", status, body)
	}

	// Roles: the phone can't mint pairings, the desk can't PATCH itself as a phone.
	status, body = h.call("POST", "/v1/vault/pairings", secondPhone, nil)
	if status != 403 || body["code"] != "role_forbidden" {
		t.Fatalf("phone minting a pairing: %d %v", status, body)
	}
	status, body = h.call("PATCH", "/v1/vault/devices/me", h.desk, map[string]any{"device_name": "x"})
	if status != 403 || body["code"] != "role_forbidden" {
		t.Fatalf("desk patching a phone: %d %v", status, body)
	}
}

func TestVaultCreationIsIdempotentAndGuardsTheKey(t *testing.T) {
	h := newSyncHarness(t)
	other := "fedcba9876543210fedcba9876543210"

	// Empty vault: a new key check is adopted.
	status, body := h.call("POST", "/v1/vault", h.desk, map[string]any{"device_name": "Test Mac", "key_check": other})
	if status != 200 || body["key_check"] != other || body["vault_id"] != h.vault {
		t.Fatalf("adopting a key on an empty vault: %d %v", status, body)
	}
	h.mustUpload("daily/2026-10-02.md", 0, "# Today\n")
	status, body = h.call("POST", "/v1/vault", h.desk, map[string]any{"device_name": "Test Mac", "key_check": testKeyCheck})
	if status != 409 || body["code"] != "key_mismatch" {
		t.Fatalf("a different key on a vault with files: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault", h.desk, map[string]any{"device_name": "Renamed Mac", "key_check": other})
	if status != 200 || body["file_count"].(float64) != 1 {
		t.Fatalf("same key again: %d %v", status, body)
	}

	// A user whose plan has no vault quota gets nothing.
	if err := h.server.store.upsertPlan(context.Background(), plan{ID: "nosync", Name: "No sync", AllowanceUnits: 10, CycleDays: 30, Models: modelTiers{Default: "x", Fast: "x"}, Limits: planLimits{WarnAtPercent: 80}}); err != nil {
		t.Fatal(err)
	}
	status, body = h.call("POST", "/admin/invites", "admin-secret", map[string]any{"plan": "nosync", "max_uses": 1})
	if status != 200 {
		t.Fatalf("invite: %d %v", status, body)
	}
	plain, _ := h.connect(body["code"].(string))
	status, body = h.call("POST", "/v1/vault", plain, map[string]any{"device_name": "Mac", "key_check": testKeyCheck})
	if status != 403 || body["code"] != "plan_excludes_sync" {
		t.Fatalf("plan without a vault: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault", plain, nil)
	if status != 404 || body["code"] != "vault_missing" {
		t.Fatalf("no vault yet: %d %v", status, body)
	}
}

func TestUploadsPublishVersionsInOrderAndRoundTripBytes(t *testing.T) {
	h := newSyncHarness(t)
	v1 := h.mustUpload("daily/2026-10-02.md", 0, "# Today\n- [ ] one\n")
	v2 := h.mustUpload("backlog.md", 0, "## Soon\n")
	if v1 != 1 || v2 != 2 {
		t.Fatalf("versions should count from 1 in commit order: %d %d", v1, v2)
	}

	// Committing the same blob again answers with the same version.
	status, body := h.call("POST", "/v1/vault/files/backlog.md/commit", h.desk, map[string]any{"expected_version": 0, "blob_id": blobIDOf("## Soon\n")})
	if status != 200 || body["version"].(float64) != 2 {
		t.Fatalf("recommit: %d %v", status, body)
	}

	// Stale expected_version carries the current row.
	status, body = h.upload("daily/2026-10-02.md", 0, "# Today\n- [x] one\n")
	if status != 409 || body["code"] != "stale_version" {
		t.Fatalf("stale: %d %v", status, body)
	}
	current := body["current"].(map[string]any)
	if current["version"].(float64) != 1 || current["blob_id"] == "" {
		t.Fatalf("stale body should name the current row: %v", current)
	}
	v3 := h.mustUpload("daily/2026-10-02.md", 1, "# Today\n- [x] one\n")
	if v3 != 3 {
		t.Fatalf("third commit should be version 3, got %d", v3)
	}

	// The phone's full pull lists live files with URLs that return the exact bytes.
	listing := h.files(h.phone, "")
	files := listing["files"].([]any)
	if len(files) != 2 || listing["has_more"] != false || listing["next_since"].(float64) != 3 {
		t.Fatalf("full pull: %v", listing)
	}
	var daily map[string]any
	for _, entry := range files {
		if entry.(map[string]any)["path"] == "daily/2026-10-02.md" {
			daily = entry.(map[string]any)
		}
	}
	response, raw := h.raw("GET", daily["download_url"].(string), "", nil, "")
	_, wantHash, wantData := blobOf("# Today\n- [x] one\n")
	if response.StatusCode != 200 || !bytes.Equal(raw, wantData) || daily["content_hash"] != wantHash {
		t.Fatalf("download should round-trip the envelope: %d %q", response.StatusCode, raw)
	}

	// Delete tombstones; incremental pulls see it, full pulls don't.
	status, body = h.call("DELETE", "/v1/vault/files/backlog.md", h.desk, map[string]any{"expected_version": 2})
	if status != 200 || body["version"].(float64) != 4 {
		t.Fatalf("delete: %d %v", status, body)
	}
	status, body = h.call("DELETE", "/v1/vault/files/backlog.md", h.desk, map[string]any{"expected_version": 99})
	if status != 200 || body["version"].(float64) != 4 {
		t.Fatalf("deleting a tombstone again: %d %v", status, body)
	}
	listing = h.files(h.phone, "?since=3")
	files = listing["files"].([]any)
	if len(files) != 1 || files[0].(map[string]any)["deleted"] != true || files[0].(map[string]any)["path"] != "backlog.md" {
		t.Fatalf("incremental pull should carry the tombstone: %v", listing)
	}
	if _, hasURL := files[0].(map[string]any)["download_url"]; hasURL {
		t.Fatalf("tombstones carry no URL: %v", files[0])
	}
	listing = h.files(h.phone, "")
	if len(listing["files"].([]any)) != 1 {
		t.Fatalf("full pull excludes tombstones: %v", listing)
	}
	listing = h.files(h.phone, "?since=4")
	if len(listing["files"].([]any)) != 0 || listing["next_since"].(float64) != 4 {
		t.Fatalf("nothing new: %v", listing)
	}

	// Paging.
	listing = h.files(h.desk, "?since=0&limit=1")
	if len(listing["files"].([]any)) != 1 || listing["has_more"] != true || listing["next_since"].(float64) != 3 {
		t.Fatalf("first page: %v", listing)
	}

	// Commit without the bytes, or with the wrong size, is refused.
	blobID, hash, data := blobOf("never uploaded")
	status, body = h.call("POST", "/v1/vault/files/notes/x.md", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash})
	if status != 200 {
		t.Fatalf("begin: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/files/notes/x.md/commit", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID})
	if status != 422 || body["code"] != "blob_missing" {
		t.Fatalf("commit without bytes: %d %v", status, body)
	}
	upload := body
	status, upload = h.call("POST", "/v1/vault/files/notes/x.md", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash})
	if status != 200 {
		t.Fatalf("begin again: %d %v", status, upload)
	}
	response, raw = h.raw("PUT", upload["upload"].(map[string]any)["url"].(string), "", data[:len(data)-1], "application/octet-stream")
	if response.StatusCode != 400 {
		t.Fatalf("short PUT should be refused: %d %s", response.StatusCode, raw)
	}
	status, body = h.call("POST", "/v1/vault/files/notes/x.md/commit", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID})
	if status != 422 || body["code"] != "blob_missing" {
		t.Fatalf("commit after a failed PUT: %d %v", status, body)
	}

	// The phone can't upload.
	status, body = h.call("POST", "/v1/vault/files/notes/y.md", h.phone, map[string]any{"expected_version": 0, "blob_id": blobID, "size_bytes": 10, "content_hash": hash})
	if status != 403 || body["code"] != "role_forbidden" {
		t.Fatalf("phone upload: %d %v", status, body)
	}
}

func TestConcurrentCommitsGetDistinctIncreasingVersions(t *testing.T) {
	h := newSyncHarness(t)
	const n = 8
	versions := make(chan int64, n)
	var wg sync.WaitGroup
	for i := range n {
		wg.Add(1)
		go func() {
			defer wg.Done()
			versions <- h.mustUpload(fmt.Sprintf("notes/%d.md", i), 0, fmt.Sprintf("note %d\n", i))
		}()
	}
	wg.Wait()
	close(versions)
	seen := map[int64]bool{}
	for v := range versions {
		if seen[v] || v < 1 || v > n {
			t.Fatalf("version %d duplicated or out of range", v)
		}
		seen[v] = true
	}
	status, body := h.call("GET", "/v1/vault", h.desk, nil)
	if status != 200 || body["latest_version"].(float64) != n || body["file_count"].(float64) != n {
		t.Fatalf("vault after concurrent commits: %v", body)
	}
}

func TestWritesQueueIdempotentlyAndAckPrunes(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("daily/2026-10-02.md", 0, "# Today\n")

	status, body := h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{"kind":"append"}`)
	if status != 201 || body["seq"].(float64) != 1 {
		t.Fatalf("first write: %d %v", status, body)
	}
	status, body = h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{"kind":"append"}`)
	if status != 200 || body["seq"].(float64) != 1 {
		t.Fatalf("retried write must answer the same seq: %d %v", status, body)
	}
	status, body = h.write("0f7e0b1a-2222-4222-8222-222222222222", "backlog.md", 0, `{"kind":"append"}`)
	if status != 201 || body["seq"].(float64) != 2 {
		t.Fatalf("second write: %d %v", status, body)
	}
	status, body = h.write("NOT-A-UUID", "backlog.md", 0, `x`)
	if status != 400 || body["code"] != "bad_request" {
		t.Fatalf("bad client id: %d %v", status, body)
	}
	status, body = h.write("0f7e0b1a-3333-4333-8333-333333333333", "photo.png", 0, `x`)
	if status != 422 || body["code"] != "path_not_allowed" {
		t.Fatalf("write to a non-text path: %d %v", status, body)
	}

	// The desk drains in seq order and the phone may not read the queue.
	status, body = h.call("GET", "/v1/vault/writes", h.desk, nil)
	if status != 200 {
		t.Fatalf("writes: %d %v", status, body)
	}
	writes := body["writes"].([]any)
	if len(writes) != 2 || writes[0].(map[string]any)["seq"].(float64) != 1 || writes[1].(map[string]any)["path"] != "backlog.md" {
		t.Fatalf("queue: %v", writes)
	}
	payload, err := base64.StdEncoding.DecodeString(writes[0].(map[string]any)["payload"].(string))
	if err != nil || string(payload) != `{"kind":"append"}` {
		t.Fatalf("payload round-trip: %q %v", payload, err)
	}
	status, body = h.call("GET", "/v1/vault/writes", h.phone, nil)
	if status != 403 || body["code"] != "role_forbidden" {
		t.Fatalf("phone reading the queue: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault", h.phone, nil)
	if body["writes"].(map[string]any)["pending"].(float64) != 2 {
		t.Fatalf("pending count: %v", body)
	}

	// Ack after the desk committed the snapshot that carries the effects.
	h.mustUpload("daily/2026-10-02.md", 1, "# Today\n- [ ] from the phone\n")
	status, body = h.call("POST", "/v1/vault/writes/ack", h.desk, map[string]any{"through_seq": 1})
	if status != 200 || body["through_seq"].(float64) != 1 || body["at_version"].(float64) != 2 {
		t.Fatalf("ack: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault/writes", h.desk, nil)
	if len(body["writes"].([]any)) != 1 {
		t.Fatalf("ack should prune seq 1: %v", body)
	}
	status, body = h.call("POST", "/v1/vault/writes/ack", h.desk, map[string]any{"through_seq": 1})
	if status != 200 || body["at_version"].(float64) != 2 {
		t.Fatalf("re-ack answers the recorded values: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/writes/ack", h.desk, map[string]any{"through_seq": 9})
	if status != 400 {
		t.Fatalf("ack beyond the queue: %d %v", status, body)
	}
	// A retry of an acked write still answers with its seq instead of queuing a duplicate.
	status, body = h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{"kind":"append"}`)
	if status != 200 || body["seq"].(float64) != 1 {
		t.Fatalf("retry after ack: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault", h.phone, nil)
	summary := body["writes"].(map[string]any)
	if summary["pending"].(float64) != 1 || summary["acked_through_seq"].(float64) != 1 || summary["acked_at_version"].(float64) != 2 {
		t.Fatalf("vault summary after ack: %v", summary)
	}
}

func TestFeedNudgesTheOtherDeviceOnly(t *testing.T) {
	h := newSyncHarness(t)
	deskEvents, closeDesk := h.feed(h.desk)
	defer closeDesk()
	phoneEvents, closePhone := h.feed(h.phone)
	defer closePhone()

	h.mustUpload("daily/2026-10-02.md", 0, "# Today\n")
	expectEvent(t, phoneEvents, `file:{"deleted":false,"path":"daily/2026-10-02.md","version":1}`)
	expectNoEvent(t, deskEvents)

	h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{}`)
	expectEvent(t, deskEvents, `write:{"path":"daily/2026-10-02.md","seq":1}`)
	expectNoEvent(t, phoneEvents)

	h.call("POST", "/v1/vault/writes/ack", h.desk, map[string]any{"through_seq": 1})
	expectEvent(t, phoneEvents, `ack:{"at_version":1,"through_seq":1}`)
	expectNoEvent(t, deskEvents)

	// With the phone listening nothing is pushed.
	if records := h.server.pusher.(*loggingPusher).records(); len(records) != 0 {
		t.Fatalf("no push while the phone holds the feed: %v", records)
	}
}

func TestPushCoalescesWhenThePhoneIsAbsent(t *testing.T) {
	h := newSyncHarness(t)
	pusher := h.server.pusher.(*loggingPusher)
	h.mustUpload("a.md", 0, "a")
	h.mustUpload("b.md", 0, "b")
	deadline := time.Now().Add(2 * time.Second)
	for len(pusher.records()) == 0 && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	records := pusher.records()
	if len(records) != 1 || records[0].Token != "aa11" {
		t.Fatalf("one push per burst to the phone's token: %v", records)
	}
	var payload map[string]any
	if err := json.Unmarshal(records[0].Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload["aps"].(map[string]any)["content-available"].(float64) != 1 || payload["thock"].(map[string]any)["vault_id"] != h.vault {
		t.Fatalf("push payload: %s", records[0].Payload)
	}
	// Past the window a new change pushes again.
	h.clock = h.clock.Add(time.Minute)
	h.mustUpload("c.md", 0, "c")
	deadline = time.Now().Add(2 * time.Second)
	for len(pusher.records()) < 2 && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	if len(pusher.records()) != 2 {
		t.Fatalf("a change in a new window should push: %d", len(pusher.records()))
	}
}

func TestQuotaRefusesAboveTheHardCap(t *testing.T) {
	h := newSyncHarness(t)
	small := testPlan
	small.Limits.VaultQuotaBytes = 100
	if err := h.server.store.upsertPlan(context.Background(), small); err != nil {
		t.Fatal(err)
	}
	// 100 bytes of quota, hard cap 110: a 60-byte envelope fits, a second one doesn't.
	h.mustUpload("a.md", 0, strings.Repeat("a", 56))
	status, body := h.upload("b.md", 0, strings.Repeat("b", 56))
	if status != 507 || body["code"] != "quota_exceeded" {
		t.Fatalf("over the cap: %d %v", status, body)
	}
	// Replacing a file counts only the growth.
	if v := h.mustUpload("a.md", 1, strings.Repeat("c", 60)); v != 2 {
		t.Fatalf("replace within cap: %d", v)
	}
	status, body = h.call("GET", "/v1/vault", h.desk, nil)
	if body["used_bytes"].(float64) != 64 {
		t.Fatalf("used bytes should track the live blob: %v", body)
	}
	// Files above 2 MB are refused outright.
	status, body = h.call("POST", "/v1/vault/files/big.md", h.desk, map[string]any{"expected_version": 0, "blob_id": strings.Repeat("ab", 16), "size_bytes": 3 << 20, "content_hash": strings.Repeat("cd", 32)})
	if status != 413 || body["code"] != "too_large" {
		t.Fatalf("3 MB: %d %v", status, body)
	}
}

func TestPathsOutsideTheAllowListAreRefused(t *testing.T) {
	bad := []string{"", "/daily/x.md", "daily/x.md/", "daily//x.md", "./x.md", "daily/../x.md", "photo.png", "notes/README",
		".thock/history/HEAD.md", ".thock/cache/x.json", ".thock/sync/state.json", ".git/config.md", strings.Repeat("a", 1025) + ".md",
		"café.md"}
	for _, path := range bad {
		if err := validateSyncPath(path); err == nil {
			t.Errorf("%q should be refused", path)
		}
	}
	good := []string{"daily/2026-10-02.md", "backlog.md", ".thock/config.toml", "routines/finance/routine.toml", "reference/Clips/A B.MD", "data.csv", "x.json", "café.md"}
	for _, path := range good {
		if err := validateSyncPath(path); err != nil {
			t.Errorf("%q should be allowed: %v", path, err)
		}
	}

	h := newSyncHarness(t)
	status, body := h.call("POST", "/v1/vault/files/photo.png", h.desk, map[string]any{"expected_version": 0, "blob_id": strings.Repeat("ab", 16), "size_bytes": 10, "content_hash": strings.Repeat("cd", 32)})
	if status != 422 || body["code"] != "path_not_allowed" {
		t.Fatalf("png: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/files/.thock/history/x.md", h.desk, map[string]any{"expected_version": 0, "blob_id": strings.Repeat("ab", 16), "size_bytes": 10, "content_hash": strings.Repeat("cd", 32)})
	if status != 422 || body["code"] != "path_not_allowed" {
		t.Fatalf("history: %d %v", status, body)
	}
	// Percent-encoded paths decode.
	if v := h.mustUpload("reference/clips/a%20b.md", 0, "clip"); v != 1 {
		t.Fatalf("encoded path: %d", v)
	}
	listing := h.files(h.phone, "")
	if listing["files"].([]any)[0].(map[string]any)["path"] != "reference/clips/a b.md" {
		t.Fatalf("decoded path: %v", listing)
	}
}

func TestLapseMakesThePhoneReadOnlyAndTheSweeperDeletesLater(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("daily/2026-10-02.md", 0, "# Today\n")
	phoneEvents, closePhone := h.feed(h.phone)
	defer closePhone()
	userID := h.user(h.desk).ID

	status, _ := h.call("POST", "/admin/users/"+userID+"/vault/lapse", "admin-secret", map[string]any{"lapsed": true})
	if status != 204 {
		t.Fatalf("lapse: %d", status)
	}
	expectEvent(t, phoneEvents, `vault:{"status":"lapsed"}`)

	status, body := h.call("GET", "/v1/vault/files", h.phone, nil)
	if status != 200 || len(body["files"].([]any)) != 1 {
		t.Fatalf("phone may still read: %d %v", status, body)
	}
	status, body = h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{}`)
	if status != 403 || body["code"] != "plus_lapsed" {
		t.Fatalf("phone write under lapse: %d %v", status, body)
	}
	status, body = h.upload("daily/2026-10-02.md", 1, "# Today\nx\n")
	if status != 403 || body["code"] != "plus_lapsed" {
		t.Fatalf("desk upload under lapse: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault", h.desk, nil)
	if status != 200 || body["status"] != "lapsed" || body["lapsed_at"] == nil {
		t.Fatalf("desk may still see the vault: %d %v", status, body)
	}

	// Not before thirty days.
	h.clock = h.clock.Add(29 * 24 * time.Hour)
	if report := h.server.sweepOnce(context.Background()); report.LapsedVaults != 0 {
		t.Fatalf("swept too early: %+v", report)
	}
	status, _ = h.call("GET", "/v1/vault", h.phone, nil)
	if status != 200 {
		t.Fatalf("vault should still be there on day 29: %d", status)
	}
	// Renewing inside the window resumes.
	status, _ = h.call("POST", "/admin/users/"+userID+"/vault/lapse", "admin-secret", map[string]any{"lapsed": false})
	if status != 204 {
		t.Fatalf("renew: %d", status)
	}
	status, body = h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{}`)
	if status != 201 {
		t.Fatalf("write after renewal: %d %v", status, body)
	}

	// Lapse again and let the thirty days pass: rows and blobs go.
	h.call("POST", "/admin/users/"+userID+"/vault/lapse", "admin-secret", map[string]any{"lapsed": true})
	h.clock = h.clock.Add(31 * 24 * time.Hour)
	report := h.server.sweepOnce(context.Background())
	if report.LapsedVaults != 1 || len(report.BlobsToDelete) != 1 {
		t.Fatalf("sweep after the window: %+v", report)
	}
	status, body = h.call("GET", "/v1/vault", h.phone, nil)
	if status != 401 {
		t.Fatalf("the phone's credential went with the vault: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault", h.desk, nil)
	if status != 404 || body["code"] != "vault_missing" {
		t.Fatalf("the desk is told there is no vault: %d %v", status, body)
	}
	if _, err := h.server.blobs.stat(context.Background(), h.vault, report.BlobsToDelete[0].BlobID); err == nil {
		t.Fatal("the blob should be deleted")
	}
}

func TestRevokingTheUserLapsesTheVault(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("daily/2026-10-02.md", 0, "# Today\n")
	status, _ := h.call("POST", "/v1/disconnect", h.desk, nil)
	if status != 204 {
		t.Fatalf("disconnect: %d", status)
	}
	status, body := h.call("GET", "/v1/vault/files", h.phone, nil)
	if status != 200 {
		t.Fatalf("phone reads after the desk disconnected: %d %v", status, body)
	}
	status, body = h.write("0f7e0b1a-1111-4111-8111-111111111111", "daily/2026-10-02.md", 1, `{}`)
	if status != 403 || body["code"] != "plus_lapsed" {
		t.Fatalf("phone write after revocation: %d %v", status, body)
	}
}

func TestTombstonesAgeOutAndExpireCursors(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("a.md", 0, "a")
	h.mustUpload("b.md", 0, "b")
	h.call("DELETE", "/v1/vault/files/a.md", h.desk, map[string]any{"expected_version": 1})
	h.clock = h.clock.Add(31 * 24 * time.Hour)
	report := h.server.sweepOnce(context.Background())
	if report.Tombstones != 1 {
		t.Fatalf("one tombstone should be pruned: %+v", report)
	}
	status, body := h.call("GET", "/v1/vault/files?since=1", h.phone, nil)
	if status != 410 || body["code"] != "cursor_expired" {
		t.Fatalf("cursor under the horizon: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault/files?since=3", h.phone, nil)
	if status != 200 {
		t.Fatalf("cursor at the horizon: %d %v", status, body)
	}
	listing := h.files(h.phone, "")
	if len(listing["files"].([]any)) != 1 {
		t.Fatalf("full pull after pruning: %v", listing)
	}
}

func TestResetRotatesTheKeyAndDisconnectsThePhone(t *testing.T) {
	h := newSyncHarness(t)
	h.mustUpload("a.md", 0, "a")
	h.write("0f7e0b1a-1111-4111-8111-111111111111", "a.md", 1, `{}`)
	other := "fedcba9876543210fedcba9876543210"
	status, body := h.call("POST", "/v1/vault/reset", h.desk, map[string]any{"key_check": other})
	if status != 200 || body["key_check"] != other || body["file_count"].(float64) != 0 || body["used_bytes"].(float64) != 0 {
		t.Fatalf("reset: %d %v", status, body)
	}
	if body["latest_version"].(float64) != 1 || body["writes"].(map[string]any)["pending"].(float64) != 0 {
		t.Fatalf("counters keep going, queue is empty: %v", body)
	}
	status, body = h.call("GET", "/v1/vault", h.phone, nil)
	if status != 401 {
		t.Fatalf("old phone after reset: %d %v", status, body)
	}
	status, body = h.call("GET", "/v1/vault/files?since=0", h.desk, nil)
	if status != 410 {
		t.Fatalf("old cursors are expired by a reset: %d %v", status, body)
	}
	if v := h.mustUpload("a.md", 0, "a again"); v != 2 {
		t.Fatalf("versions continue after reset: %d", v)
	}
	phone := h.pairPhone("New iPhone", "")
	status, body = h.call("GET", "/v1/vault", phone, nil)
	if status != 200 || body["key_check"] != other {
		t.Fatalf("new phone: %d %v", status, body)
	}

	// Disconnect phone and turn sync off.
	deviceID := ""
	for _, d := range body["devices"].([]any) {
		if d.(map[string]any)["role"] == "phone" {
			deviceID = d.(map[string]any)["device_id"].(string)
		}
	}
	status, _ = h.call("POST", "/v1/vault/devices/"+deviceID+"/revoke", h.desk, nil)
	if status != 204 {
		t.Fatalf("revoke phone: %d", status)
	}
	status, _ = h.call("GET", "/v1/vault", phone, nil)
	if status != 401 {
		t.Fatalf("revoked phone: %d", status)
	}
	status, _ = h.call("DELETE", "/v1/vault", h.desk, nil)
	if status != 204 {
		t.Fatalf("turn sync off: %d", status)
	}
	status, body = h.call("GET", "/v1/vault", h.desk, nil)
	if status != 404 || body["code"] != "vault_missing" {
		t.Fatalf("after turning sync off: %d %v", status, body)
	}
	if _, err := h.server.blobs.stat(context.Background(), h.vault, blobIDOf("a again")); err == nil {
		t.Fatal("blobs go with the vault")
	}
}

func blobIDOf(content string) string {
	id, _, _ := blobOf(content)
	return id
}

func TestStaleUploadsAreSwept(t *testing.T) {
	h := newSyncHarness(t)
	blobID, hash, data := blobOf("abandoned")
	status, body := h.call("POST", "/v1/vault/files/x.md", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID, "size_bytes": len(data), "content_hash": hash})
	if status != 200 {
		t.Fatalf("begin: %d %v", status, body)
	}
	h.raw("PUT", body["upload"].(map[string]any)["url"].(string), "", data, "application/octet-stream")
	h.clock = h.clock.Add(2 * time.Hour)
	report := h.server.sweepOnce(context.Background())
	if report.StaleUploads != 1 {
		t.Fatalf("stale upload should be swept: %+v", report)
	}
	if _, err := h.server.blobs.stat(context.Background(), h.vault, blobID); err == nil {
		t.Fatal("the abandoned blob should be gone")
	}
	status, body = h.call("POST", "/v1/vault/files/x.md/commit", h.desk, map[string]any{"expected_version": 0, "blob_id": blobID})
	if status != 422 || body["code"] != "blob_missing" {
		t.Fatalf("commit of a swept upload: %d %v", status, body)
	}
}
