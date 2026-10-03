package main

import (
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"
)

func TestStaticFilesCantEscapePublic(t *testing.T) {
	f := newFixture(t)
	for _, path := range []string{
		"/../main.go", "/%2e%2e/main.go", "/..%2fmain.go", "/public/../main.go", "/main.go", "/go.mod", "/seal.mjs",
		"/./../../etc/passwd", "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
	} {
		response := f.get(canonicalHost, path)
		body := response.Body.String()
		if response.Code == http.StatusOK || strings.Contains(body, "package main") || strings.Contains(body, "root:") {
			t.Errorf("%s: %d %.80q", path, response.Code, body)
		}
	}
}

func TestOnlyTheWWWHostRedirects(t *testing.T) {
	f := newFixture(t)
	for _, host := range []string{"evil.example", "www.thethock.com.evil.example", "evil.example/www.thethock.com", "thethock.com"} {
		response := f.get(host, "/download")
		if location := response.Header().Get("Location"); location != "" {
			t.Errorf("Host %q redirected to %q", host, location)
		}
	}
	response := f.get("WWW.THETHOCK.COM", "//evil.example/x")
	if location := response.Header().Get("Location"); !strings.HasPrefix(location, "https://"+canonicalHost+"/") {
		t.Errorf("the www redirect left the canonical host: %q", location)
	}
}

func TestWaitlistBodiesAreBounded(t *testing.T) {
	f := newFixture(t)
	response := f.post("/waitlist", `{"email":"ada@example.com","padding":"`+strings.Repeat("x", maxSignupBody)+`"}`)
	if response.Code != http.StatusBadRequest {
		t.Fatalf("an oversized signup: %d %s", response.Code, response.Body.String())
	}
	response = f.post("/waitlist", `{"email":"`+strings.Repeat("a", 250)+`@example.com"}`)
	if response.Code != http.StatusBadRequest {
		t.Fatalf("an over-long address: %d %s", response.Code, response.Body.String())
	}
	if len(f.firestore.requests) != 0 {
		t.Fatalf("refused signups reached Firestore: %d requests", len(f.firestore.requests))
	}
}

func TestSignupAddressesCantSteerTheFirestoreRequest(t *testing.T) {
	f := newFixture(t)
	for _, email := range []string{
		"a&documentId=admin@example.com",
		"a/../../users@example.com",
		"a%2F..%2Fx@example.com",
		"a?x=1#@example.com",
	} {
		f.post("/waitlist", `{"email":"`+email+`"}`)
	}
	if len(f.firestore.requests) == 0 {
		t.Fatal("no hostile address got as far as Firestore, so nothing was checked")
	}
	documentID := regexp.MustCompile(`^[0-9a-f]{32}$`)
	for _, request := range f.firestore.requests {
		if !strings.HasSuffix(request.URL.Path, "/documents/waitlist") {
			t.Errorf("a signup wrote to %q", request.URL.Path)
		}
		query := request.URL.Query()
		if len(query) != 1 || !documentID.MatchString(query.Get("documentId")) {
			t.Errorf("a signup carried the query %q", request.URL.RawQuery)
		}
	}
}

func TestErrorsDontEchoFirestoreDetails(t *testing.T) {
	f := newFixture(t)
	f.firestore.fail = true
	response := f.post("/waitlist", `{"email":"ada@example.com"}`)
	if response.Code != http.StatusServiceUnavailable || strings.Contains(response.Body.String(), "boom") {
		t.Fatalf("a Firestore failure: %d %s", response.Code, response.Body.String())
	}
}

func TestStaticResponsesAreNotSniffable(t *testing.T) {
	f := newFixture(t)
	for _, path := range []string{"/", "/download", "/gate.json"} {
		response := f.get(canonicalHost, path)
		if got := response.Header().Get("X-Content-Type-Options"); got != "nosniff" {
			t.Errorf("%s: X-Content-Type-Options = %q", path, got)
		}
	}
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodPut, "http://"+canonicalHost+"/waitlist", strings.NewReader(`{}`))
	request.Host = canonicalHost
	f.handler.ServeHTTP(recorder, request)
	if recorder.Code != http.StatusMethodNotAllowed {
		t.Errorf("PUT /waitlist: %d", recorder.Code)
	}
}
