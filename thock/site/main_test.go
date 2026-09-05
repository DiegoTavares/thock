package main

import (
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func testHandler(t *testing.T) http.Handler {
	t.Helper()
	files, err := fs.Sub(publicFiles, "public")
	if err != nil {
		t.Fatal(err)
	}
	return handler(files)
}

func get(t *testing.T, h http.Handler, host, path string) *httptest.ResponseRecorder {
	t.Helper()
	request := httptest.NewRequest(http.MethodGet, "http://"+host+path, nil)
	request.Host = host
	recorder := httptest.NewRecorder()
	h.ServeHTTP(recorder, request)
	return recorder
}

func TestServesLandingPage(t *testing.T) {
	response := get(t, testHandler(t), canonicalHost, "/")
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
	h := testHandler(t)
	for _, path := range []string{"/download", "/download.html"} {
		response := get(t, h, canonicalHost, path)
		if response.Code != http.StatusOK {
			t.Fatalf("%s: status %d", path, response.Code)
		}
		if !strings.Contains(response.Body.String(), "data-gate") {
			t.Fatalf("%s: download page not served", path)
		}
	}
}

func TestGateBlobIsServed(t *testing.T) {
	response := get(t, testHandler(t), canonicalHost, "/gate.json")
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
	response := get(t, testHandler(t), canonicalHost, "/nope")
	if response.Code != http.StatusNotFound {
		t.Fatalf("status %d", response.Code)
	}
}

func TestHealth(t *testing.T) {
	response := get(t, testHandler(t), canonicalHost, "/health")
	if response.Code != http.StatusOK || response.Body.String() != "ok" {
		t.Fatalf("status %d body %q", response.Code, response.Body.String())
	}
}

func TestWWWRedirectsToApex(t *testing.T) {
	response := get(t, testHandler(t), "www."+canonicalHost, "/download?from=mail")
	if response.Code != http.StatusMovedPermanently {
		t.Fatalf("status %d", response.Code)
	}
	if got := response.Header().Get("Location"); got != "https://"+canonicalHost+"/download?from=mail" {
		t.Fatalf("Location = %q", got)
	}
}
