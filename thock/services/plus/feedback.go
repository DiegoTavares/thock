package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

// Problem reports from the phone (thock/specs/v39-phone-problem-reports.md):
// a description, the connection facts the phone knows and up to a few
// screenshots become one issue on a private GitHub repository. The phone
// never learns where the issue went; it gets a number back and a thank you.

const (
	feedbackMaxDescription = 4000
	feedbackMaxDetails     = 8000
	feedbackMaxScreenshots = 3
	feedbackMaxImageBytes  = 2 << 20
	// Base64 of three images plus the text, with room to spare.
	feedbackMaxBodyBytes = 10 << 20
	feedbackPerHour      = 5
)

// issueTracker is where reports end up. GitHub in production, a recorder in
// tests.
type issueTracker interface {
	// putFile stores bytes at a path in the tracker's repository and returns
	// a URL that renders inside an issue there.
	putFile(ctx context.Context, path string, content []byte, message string) (string, error)
	createIssue(ctx context.Context, title, body string, labels []string) (int, error)
}

type feedbackRequest struct {
	Description string               `json:"description"`
	Details     string               `json:"details"`
	AppVersion  string               `json:"app_version"`
	Build       string               `json:"build"`
	System      string               `json:"system"`
	Screenshots []feedbackScreenshot `json:"screenshots"`
}

type feedbackScreenshot struct {
	ContentType string `json:"content_type"`
	Data        string `json:"data"`
}

// feedbackRoute lets a phone whose Plus has lapsed still ask for help.
var feedbackRoute = routeAccess{roles: []deviceRole{rolePhone}, needVault: true, lapsedPhone: true}

func (s *server) handleFeedback(w http.ResponseWriter, r *http.Request, p principal) {
	if s.issues == nil {
		writeErrorCode(w, http.StatusServiceUnavailable, "feedback_unavailable", "Reporting a problem isn't set up on this server yet.")
		return
	}
	var request feedbackRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, feedbackMaxBodyBytes)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The report wasn't readable.")
		return
	}
	description := strings.TrimSpace(request.Description)
	if description == "" {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "Say what happened first.")
		return
	}
	if utf8.RuneCountInString(description) > feedbackMaxDescription {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "That description is too long. Keep it under 4000 characters.")
		return
	}
	if utf8.RuneCountInString(request.Details) > feedbackMaxDetails {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The details are too long.")
		return
	}
	if len(request.Screenshots) > feedbackMaxScreenshots {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "Up to three screenshots.")
		return
	}
	images := make([][]byte, 0, len(request.Screenshots))
	kinds := make([]string, 0, len(request.Screenshots))
	for _, shot := range request.Screenshots {
		raw, err := base64.StdEncoding.DecodeString(shot.Data)
		if err != nil || len(raw) == 0 {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "A screenshot wasn't readable.")
			return
		}
		if len(raw) > feedbackMaxImageBytes {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "A screenshot is too large. Each one must be under 2 MB.")
			return
		}
		// The bytes decide the kind, not the declared type.
		kind := http.DetectContentType(raw)
		if kind != "image/png" && kind != "image/jpeg" {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "Screenshots must be PNG or JPEG images.")
			return
		}
		images = append(images, raw)
		kinds = append(kinds, kind)
	}
	if !s.feedbackLimiter.allow(p.device.ID, s.now()) {
		writeErrorCode(w, http.StatusTooManyRequests, "too_many_reports", "That's a few reports in a row. Give it an hour, or write to us directly.")
		return
	}

	now := s.now().UTC()
	folder := fmt.Sprintf("reports/%s/%s-%s", now.Format("2006-01-02"), now.Format("150405"), shortID(p.device.ID))
	var screenshots []string
	for index, raw := range images {
		extension := "png"
		if kinds[index] == "image/jpeg" {
			extension = "jpg"
		}
		path := fmt.Sprintf("%s/%d.%s", folder, index+1, extension)
		url, err := s.issues.putFile(r.Context(), path, raw, "Screenshot for a phone report")
		if err != nil {
			logf("error: storing a report screenshot: %v", err)
			writeErrorCode(w, http.StatusBadGateway, "feedback_failed", "Couldn't send the report right now. Try again in a moment.")
			return
		}
		screenshots = append(screenshots, url)
	}
	title, body := feedbackIssue(request, description, screenshots, p, now)
	number, err := s.issues.createIssue(r.Context(), title, body, []string{"phone"})
	if err != nil {
		logf("error: filing a report: %v", err)
		writeErrorCode(w, http.StatusBadGateway, "feedback_failed", "Couldn't send the report right now. Try again in a moment.")
		return
	}
	logf("notice: phone report #%d from vault %s", number, p.vault.ID)
	writeJSON(w, http.StatusCreated, map[string]any{"number": number})
}

// feedbackIssue writes the issue the way a reader triages it: the person's
// words first, the pictures, then the facts in a block that keeps their
// shape.
func feedbackIssue(request feedbackRequest, description string, screenshots []string, p principal, now time.Time) (title, body string) {
	first := description
	if cut := strings.IndexAny(first, "\r\n"); cut >= 0 {
		first = first[:cut]
	}
	first = strings.TrimSpace(first)
	if utf8.RuneCountInString(first) > 72 {
		runes := []rune(first)
		first = strings.TrimSpace(string(runes[:72])) + "…"
	}
	title = "Phone: " + first

	var out strings.Builder
	out.WriteString("## What happened\n\n")
	out.WriteString(description)
	out.WriteString("\n")
	if len(screenshots) > 0 {
		out.WriteString("\n## Screenshots\n\n")
		for index, url := range screenshots {
			fmt.Fprintf(&out, "![screenshot %d](%s)\n", index+1, url)
		}
	}
	out.WriteString("\n## Details\n\n```\n")
	fmt.Fprintf(&out, "Thock for iPhone %s (%s) · %s\n", blankAs(request.AppVersion, "?"), blankAs(request.Build, "?"), blankAs(request.System, "?"))
	fmt.Fprintf(&out, "Reported: %s\n", now.Format(time.RFC3339))
	fmt.Fprintf(&out, "Vault: %s · phone: %s (%s)\n", p.vault.ID, p.device.ID, p.device.Name)
	if details := strings.TrimSpace(request.Details); details != "" {
		out.WriteString(details)
		out.WriteString("\n")
	}
	out.WriteString("```\n")
	return title, out.String()
}

func blankAs(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

func shortID(id string) string {
	id = strings.ReplaceAll(id, "-", "")
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// feedbackLimiter counts reports per phone over the last hour. In memory,
// which is enough because the service runs one instance.
type feedbackLimiter struct {
	mu    sync.Mutex
	sent  map[string][]time.Time
	limit int
}

func newFeedbackLimiter(limit int) *feedbackLimiter {
	return &feedbackLimiter{sent: map[string][]time.Time{}, limit: limit}
}

func (l *feedbackLimiter) allow(key string, now time.Time) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	recent := l.sent[key][:0]
	for _, at := range l.sent[key] {
		if now.Sub(at) < time.Hour {
			recent = append(recent, at)
		}
	}
	if len(recent) >= l.limit {
		l.sent[key] = recent
		return false
	}
	l.sent[key] = append(recent, now)
	return true
}

// githubTracker files issues and stores screenshots in one repository, by
// its REST API. Screenshots are committed to the repository and embedded by
// the URL GitHub renders for anyone who can read it.
type githubTracker struct {
	base  string
	repo  string
	token string
	http  *http.Client
}

func newGitHubTracker(repo, token, base string) *githubTracker {
	return &githubTracker{base: strings.TrimRight(base, "/"), repo: repo, token: token, http: &http.Client{Timeout: 20 * time.Second}}
}

func (g *githubTracker) putFile(ctx context.Context, path string, content []byte, message string) (string, error) {
	payload := map[string]string{"message": message, "content": base64.StdEncoding.EncodeToString(content)}
	var response struct {
		Content struct {
			HTMLURL string `json:"html_url"`
		} `json:"content"`
	}
	if err := g.call(ctx, "PUT", "/repos/"+g.repo+"/contents/"+path, payload, &response); err != nil {
		return "", err
	}
	if response.Content.HTMLURL == "" {
		return "", fmt.Errorf("github: no url for %s", path)
	}
	return response.Content.HTMLURL + "?raw=true", nil
}

func (g *githubTracker) createIssue(ctx context.Context, title, body string, labels []string) (int, error) {
	payload := map[string]any{"title": title, "body": body, "labels": labels}
	var response struct {
		Number int `json:"number"`
	}
	if err := g.call(ctx, "POST", "/repos/"+g.repo+"/issues", payload, &response); err != nil {
		return 0, err
	}
	return response.Number, nil
}

func (g *githubTracker) call(ctx context.Context, method, path string, payload any, out any) error {
	raw, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	request, err := http.NewRequestWithContext(ctx, method, g.base+path, bytes.NewReader(raw))
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+g.token)
	request.Header.Set("Accept", "application/vnd.github+json")
	request.Header.Set("X-GitHub-Api-Version", "2022-11-28")
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("User-Agent", "thock-plus")
	response, err := g.http.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return err
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("github: %s %s answered %d: %s", method, path, response.StatusCode, strings.TrimSpace(string(body)))
	}
	return json.Unmarshal(body, out)
}
