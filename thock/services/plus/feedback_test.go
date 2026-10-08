package main

import (
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

// A stand-in for api.github.com that remembers what was filed.
type fakeGitHub struct {
	mu     sync.Mutex
	files  map[string][]byte
	issues []map[string]any
	fail   bool
}

func newFakeGitHub(t *testing.T) (*fakeGitHub, *httptest.Server) {
	t.Helper()
	g := &fakeGitHub{files: map[string][]byte{}}
	mux := http.NewServeMux()
	mux.HandleFunc("PUT /repos/{owner}/{repo}/contents/{path...}", func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer ghp_test" {
			http.Error(w, "bad token", http.StatusUnauthorized)
			return
		}
		var body struct {
			Message string `json:"message"`
			Content string `json:"content"`
		}
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		raw, err := base64.StdEncoding.DecodeString(body.Content)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		g.mu.Lock()
		g.files[r.PathValue("path")] = raw
		g.mu.Unlock()
		writeJSON(w, http.StatusCreated, map[string]any{"content": map[string]any{
			"html_url": "https://github.com/" + r.PathValue("owner") + "/" + r.PathValue("repo") + "/blob/main/" + r.PathValue("path"),
		}})
	})
	mux.HandleFunc("POST /repos/{owner}/{repo}/issues", func(w http.ResponseWriter, r *http.Request) {
		g.mu.Lock()
		defer g.mu.Unlock()
		if g.fail {
			http.Error(w, "down", http.StatusBadGateway)
			return
		}
		var issue map[string]any
		if err := json.NewDecoder(r.Body).Decode(&issue); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		g.issues = append(g.issues, issue)
		writeJSON(w, http.StatusCreated, map[string]any{"number": len(g.issues) + 40})
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return g, server
}

var tinyPNG = append([]byte("\x89PNG\r\n\x1a\n"), make([]byte, 64)...)

func report(description string, screenshots ...[]byte) map[string]any {
	shots := []map[string]any{}
	for _, shot := range screenshots {
		shots = append(shots, map[string]any{"content_type": "image/png", "data": base64.StdEncoding.EncodeToString(shot)})
	}
	return map[string]any{
		"description": description,
		"details":     "Status: can't reach the desk's copy\nLast check: 2026-10-07T20:02:00Z",
		"app_version": "1.0",
		"build":       "11",
		"system":      "iOS 26.0 · iPhone17,1",
		"screenshots": shots,
	}
}

func TestPhoneReportBecomesAnIssueWithItsScreenshots(t *testing.T) {
	h := newSyncHarness(t)
	github, api := newFakeGitHub(t)
	h.server.issues = newGitHubTracker("DiegoTavares/thock-feedback", "ghp_test", api.URL)

	status, body := h.call("POST", "/v1/vault/feedback", h.phone, report("Captures stopped reaching my desk\nSince this morning, nothing arrives.", tinyPNG))
	if status != 201 || body["number"].(float64) != 41 {
		t.Fatalf("report: %d %v", status, body)
	}
	if len(github.issues) != 1 {
		t.Fatalf("one issue expected: %v", github.issues)
	}
	issue := github.issues[0]
	if issue["title"] != "Phone: Captures stopped reaching my desk" {
		t.Errorf("title: %q", issue["title"])
	}
	if labels := issue["labels"].([]any); len(labels) != 1 || labels[0] != "phone" {
		t.Errorf("labels: %v", labels)
	}
	text := issue["body"].(string)
	for _, want := range []string{
		"## What happened\n\nCaptures stopped reaching my desk\nSince this morning, nothing arrives.\n",
		"![screenshot 1](https://github.com/DiegoTavares/thock-feedback/blob/main/reports/2026-09-11/120000-",
		"/1.png?raw=true)",
		"Thock for iPhone 1.0 (11) · iOS 26.0 · iPhone17,1",
		"Reported: 2026-09-11T12:00:00Z",
		"Vault: " + h.vault + " · phone: ",
		"(Test iPhone)",
		"Status: can't reach the desk's copy\nLast check: 2026-10-07T20:02:00Z\n```",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("issue body lacks %q:\n%s", want, text)
		}
	}
	if len(github.files) != 1 {
		t.Fatalf("one screenshot committed: %v", github.files)
	}
	for path, raw := range github.files {
		if !strings.HasPrefix(path, "reports/2026-09-11/120000-") || !strings.HasSuffix(path, "/1.png") || string(raw) != string(tinyPNG) {
			t.Errorf("screenshot at %q (%d bytes)", path, len(raw))
		}
	}
}

func TestPhoneReportsAreCheckedBeforeAnythingIsFiled(t *testing.T) {
	h := newSyncHarness(t)
	github, api := newFakeGitHub(t)
	h.server.issues = newGitHubTracker("DiegoTavares/thock-feedback", "ghp_test", api.URL)

	cases := []struct {
		name string
		body map[string]any
	}{
		{"empty description", report("   ")},
		{"long description", report(strings.Repeat("x", feedbackMaxDescription+1))},
		{"not an image", report("The app froze", []byte("<html>not a picture</html>"))},
		{"too many screenshots", report("The app froze", tinyPNG, tinyPNG, tinyPNG, tinyPNG)},
		{"oversized screenshot", report("The app froze", append(append([]byte{}, tinyPNG...), make([]byte, feedbackMaxImageBytes)...))},
	}
	for _, c := range cases {
		status, body := h.call("POST", "/v1/vault/feedback", h.phone, c.body)
		if status != 400 || body["code"] != "bad_request" {
			t.Errorf("%s: %d %v", c.name, status, body)
		}
	}
	if len(github.issues) != 0 || len(github.files) != 0 {
		t.Fatalf("nothing should have been filed: %v %v", github.issues, github.files)
	}
}

func TestPhoneReportsNeedThePhoneAndATracker(t *testing.T) {
	h := newSyncHarness(t)

	status, body := h.call("POST", "/v1/vault/feedback", h.phone, report("Nothing arrives"))
	if status != 503 || body["code"] != "feedback_unavailable" {
		t.Fatalf("without a tracker: %d %v", status, body)
	}
	_, api := newFakeGitHub(t)
	h.server.issues = newGitHubTracker("DiegoTavares/thock-feedback", "ghp_test", api.URL)
	status, body = h.call("POST", "/v1/vault/feedback", h.desk, report("Nothing arrives"))
	if status != 403 || body["code"] != "role_forbidden" {
		t.Fatalf("from the desk: %d %v", status, body)
	}
	status, body = h.call("POST", "/v1/vault/feedback", "", report("Nothing arrives"))
	if status != 401 {
		t.Fatalf("without a credential: %d %v", status, body)
	}
}

func TestPhoneReportsAreRateLimitedPerPhoneAndSurviveAnOutage(t *testing.T) {
	h := newSyncHarness(t)
	github, api := newFakeGitHub(t)
	h.server.issues = newGitHubTracker("DiegoTavares/thock-feedback", "ghp_test", api.URL)

	for i := 0; i < feedbackPerHour; i++ {
		if status, body := h.call("POST", "/v1/vault/feedback", h.phone, report("Nothing arrives")); status != 201 {
			t.Fatalf("report %d: %d %v", i+1, status, body)
		}
	}
	status, body := h.call("POST", "/v1/vault/feedback", h.phone, report("Nothing arrives"))
	if status != 429 || body["code"] != "too_many_reports" {
		t.Fatalf("one too many: %d %v", status, body)
	}
	h.clock = h.clock.Add(time.Hour + time.Minute)
	github.fail = true
	status, body = h.call("POST", "/v1/vault/feedback", h.phone, report("Nothing arrives"))
	if status != 502 || body["code"] != "feedback_failed" {
		t.Fatalf("with GitHub down: %d %v", status, body)
	}
	if len(github.issues) != feedbackPerHour {
		t.Fatalf("%d issues filed", len(github.issues))
	}
}
