package main

import (
	"net/http"
	"strings"
	"sync"
	"testing"
)

func TestHostileChannelNamesNeverReachTheBucket(t *testing.T) {
	var mu sync.Mutex
	var fetched []string
	_, mux := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		fetched = append(fetched, r.URL.Path)
		mu.Unlock()
		http.NotFound(w, r)
	})
	for _, channel := range []string{
		"..", "%2e%2e", "..%2Fdist", "a%2F..%2F..%2Fsecret", "%2Fetc", "Stable", "-x", ".hidden", "a%00b", "a%3Fx=1",
	} {
		response := get(mux, "/releases/"+channel+"/latest/asset?asset=thock&os=macos&arch=aarch64")
		if response.Code == http.StatusOK {
			t.Errorf("channel %q answered 200", channel)
		}
		get(mux, "/releases/"+channel+"/1.0.0")
	}
	mu.Lock()
	defer mu.Unlock()
	if len(fetched) != 0 {
		t.Errorf("hostile channels reached the bucket: %v", fetched)
	}
}

func TestReleaseNotesOnlyRedirectToTheManifestsURL(t *testing.T) {
	_, mux := newTestServer(t, stableBucket(t))
	response := get(mux, "/releases/stable/1.16.0?next=https://evil.example")
	if response.Code != http.StatusFound {
		t.Fatalf("status %d", response.Code)
	}
	if location := response.Header().Get("Location"); !strings.HasPrefix(location, "https://github.com/DiegoTavares/thock/") {
		t.Fatalf("redirected to %q", location)
	}
}

func TestOversizedManifestsAreNotTrusted(t *testing.T) {
	_, mux := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		// The client stops reading at its limit, so this write may fail.
		_, _ = w.Write([]byte(`{"version":"9.9.9","assets":[],"padding":"` + strings.Repeat("x", 2<<20) + `"}`))
	})
	response := get(mux, "/releases/stable/latest/asset?asset=thock&os=macos&arch=aarch64")
	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("a manifest over the read limit: %d %s", response.Code, response.Body.String())
	}
}
