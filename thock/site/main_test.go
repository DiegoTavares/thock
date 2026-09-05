package main

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

// A stand-in for Firestore's createDocument: remembers document ids and answers
// 409 on a repeat, like the real thing. `fail` makes every call a 500.
type fakeFirestore struct {
	mu       sync.Mutex
	created  map[string]map[string]any
	requests []*http.Request
	fail     bool
}

func (f *fakeFirestore) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.requests = append(f.requests, r)
	if f.fail {
		http.Error(w, `{"error":{"message":"boom"}}`, http.StatusInternalServerError)
		return
	}
	id := r.URL.Query().Get("documentId")
	if _, exists := f.created[id]; exists {
		http.Error(w, `{"error":{"status":"ALREADY_EXISTS"}}`, http.StatusConflict)
		return
	}
	var body map[string]any
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	f.created[id] = body
	w.WriteHeader(http.StatusOK)
}

type fixture struct {
	handler   http.Handler
	firestore *fakeFirestore
	logs      *bytes.Buffer
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	files, err := fs.Sub(publicFiles, "public")
	if err != nil {
		t.Fatal(err)
	}
	store := &fakeFirestore{created: map[string]map[string]any{}}
	server := httptest.NewServer(store)
	t.Cleanup(server.Close)
	logs := &bytes.Buffer{}
	signups := &waitlist{
		documentsBase: server.URL + "/v1/projects/test/databases/(default)/documents",
		token:         func(context.Context) (string, error) { return "test-token", nil },
		client:        server.Client(),
		now:           func() time.Time { return time.Date(2026, 9, 5, 12, 0, 0, 0, time.UTC) },
		logs:          logs,
	}
	return &fixture{handler: handler(files, signups), firestore: store, logs: logs}
}

func (f *fixture) get(host, path string) *httptest.ResponseRecorder {
	request := httptest.NewRequest(http.MethodGet, "http://"+host+path, nil)
	request.Host = host
	recorder := httptest.NewRecorder()
	f.handler.ServeHTTP(recorder, request)
	return recorder
}

func (f *fixture) post(path, body string) *httptest.ResponseRecorder {
	request := httptest.NewRequest(http.MethodPost, "http://"+canonicalHost+path, strings.NewReader(body))
	request.Host = canonicalHost
	request.Header.Set("Content-Type", "application/json")
	recorder := httptest.NewRecorder()
	f.handler.ServeHTTP(recorder, request)
	return recorder
}

func TestServesLandingPage(t *testing.T) {
	response := newFixture(t).get(canonicalHost, "/")
	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if !strings.Contains(response.Body.String(), "<title>Thock</title>") {
		t.Fatalf("index.html not served: %.200s", response.Body.String())
	}
	if got := response.Header().Get("Cache-Control"); got != "public, max-age=300" {
		t.Fatalf("Cache-Control = %q", got)
	}
}

func TestDownloadCleanURL(t *testing.T) {
	f := newFixture(t)
	for _, path := range []string{"/download", "/download.html"} {
		response := f.get(canonicalHost, path)
		if response.Code != http.StatusOK {
			t.Fatalf("%s: status %d", path, response.Code)
		}
		if !strings.Contains(response.Body.String(), "data-gate") {
			t.Fatalf("%s: download page not served", path)
		}
	}
}

func TestGateBlobIsServed(t *testing.T) {
	response := newFixture(t).get(canonicalHost, "/gate.json")
	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	body := response.Body.String()
	for _, field := range []string{`"salt"`, `"iv"`, `"ciphertext"`, `"iterations"`} {
		if !strings.Contains(body, field) {
			t.Fatalf("gate.json missing %s: %s", field, body)
		}
	}
	// The whole point of the gate: the manifest location is never in plaintext.
	if strings.Contains(body, "storage.googleapis.com") {
		t.Fatal("gate.json leaks the manifest URL")
	}
}

func TestUnknownPathIs404(t *testing.T) {
	response := newFixture(t).get(canonicalHost, "/nope")
	if response.Code != http.StatusNotFound {
		t.Fatalf("status %d", response.Code)
	}
}

func TestHealth(t *testing.T) {
	response := newFixture(t).get(canonicalHost, "/health")
	if response.Code != http.StatusOK || response.Body.String() != "ok" {
		t.Fatalf("status %d body %q", response.Code, response.Body.String())
	}
}

func TestWWWRedirectsToApex(t *testing.T) {
	response := newFixture(t).get("www."+canonicalHost, "/download?from=mail")
	if response.Code != http.StatusMovedPermanently {
		t.Fatalf("status %d", response.Code)
	}
	if got := response.Header().Get("Location"); got != "https://"+canonicalHost+"/download?from=mail" {
		t.Fatalf("Location = %q", got)
	}
}

func TestSignupCreatesDocumentAndLogs(t *testing.T) {
	f := newFixture(t)
	response := f.post("/waitlist", `{"email":"  Ada@Example.COM "}`)
	if response.Code != http.StatusOK {
		t.Fatalf("status %d body %s", response.Code, response.Body.String())
	}
	id := documentID("ada@example.com")
	document, ok := f.firestore.created[id]
	if !ok {
		t.Fatalf("no document %s; created: %v", id, f.firestore.created)
	}
	fields := document["fields"].(map[string]any)
	if got := fields["email"].(map[string]any)["stringValue"]; got != "ada@example.com" {
		t.Fatalf("email field = %v", got)
	}
	if got := fields["joined_at"].(map[string]any)["timestampValue"]; got != "2026-09-05T12:00:00Z" {
		t.Fatalf("joined_at = %v", got)
	}
	if got := f.firestore.requests[0].Header.Get("Authorization"); got != "Bearer test-token" {
		t.Fatalf("Authorization = %q", got)
	}
	var entry map[string]any
	if err := json.NewDecoder(f.logs).Decode(&entry); err != nil {
		t.Fatalf("log entry: %v (%q)", err, f.logs.String())
	}
	if entry["event"] != "waitlist_signup" || entry["email"] != "ada@example.com" || entry["severity"] != "NOTICE" {
		t.Fatalf("log entry = %v", entry)
	}
}

func TestRepeatSignupIsQuietlyOK(t *testing.T) {
	f := newFixture(t)
	f.post("/waitlist", `{"email":"ada@example.com"}`)
	f.logs.Reset()
	response := f.post("/waitlist", `{"email":"ADA@example.com"}`)
	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if len(f.firestore.created) != 1 {
		t.Fatalf("created %d documents", len(f.firestore.created))
	}
	if !strings.Contains(f.logs.String(), `"waitlist_repeat"`) {
		t.Fatalf("repeat not logged as such: %s", f.logs.String())
	}
}

func TestSignupRejectsBadInput(t *testing.T) {
	f := newFixture(t)
	for _, body := range []string{`{"email":"not-an-address"}`, `{"email":""}`, `{}`, `not json`, `{"email":"` + strings.Repeat("a", 300) + `@x.io"}`} {
		response := f.post("/waitlist", body)
		if response.Code != http.StatusBadRequest {
			t.Fatalf("%.40s: status %d", body, response.Code)
		}
		var reply map[string]any
		if err := json.Unmarshal(response.Body.Bytes(), &reply); err != nil || reply["error"] == "" {
			t.Fatalf("%.40s: body %s", body, response.Body.String())
		}
	}
	if len(f.firestore.requests) != 0 {
		t.Fatal("bad input reached Firestore")
	}
}

func TestSignupWhenFirestoreIsDown(t *testing.T) {
	f := newFixture(t)
	f.firestore.fail = true
	response := f.post("/waitlist", `{"email":"ada@example.com"}`)
	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("status %d", response.Code)
	}
	body, _ := io.ReadAll(response.Body)
	if !strings.Contains(string(body), "trying again") {
		t.Fatalf("body %s", body)
	}
	if !strings.Contains(f.logs.String(), `"waitlist_error"`) {
		t.Fatalf("failure not logged: %s", f.logs.String())
	}
}

func TestSignupIsPostOnly(t *testing.T) {
	response := newFixture(t).get(canonicalHost, "/waitlist")
	if response.Code != http.StatusMethodNotAllowed {
		t.Fatalf("status %d", response.Code)
	}
}
