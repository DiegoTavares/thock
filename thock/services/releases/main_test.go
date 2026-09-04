package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const stableManifest = `{
  "version": "1.16.0",
  "released_at": "2026-09-10T12:00:00Z",
  "notes_url": "https://github.com/DiegoTavares/thock/releases/tag/v1.16.0",
  "assets": [
    {"asset": "thock", "os": "macos", "arch": "aarch64",
     "url": "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
     "sha256": "abc"},
    {"asset": "thock", "os": "linux", "arch": "x86_64",
     "url": "https://storage.googleapis.com/thock-releases/dist/v1.16.0/thock-linux-x86_64.tar.gz",
     "sha256": "def"}
  ]
}`

// newTestServer wires the service to an httptest stand-in for the bucket.
func newTestServer(t *testing.T, bucket http.HandlerFunc) (*server, *http.ServeMux) {
	t.Helper()
	bucketServer := httptest.NewServer(bucket)
	t.Cleanup(bucketServer.Close)
	s := newServer(bucketServer.URL)
	return s, s.routes()
}

func stableBucket(t *testing.T) http.HandlerFunc {
	t.Helper()
	return func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/channels/stable.json" {
			if _, err := w.Write([]byte(stableManifest)); err != nil {
				t.Errorf("writing manifest: %v", err)
			}
			return
		}
		http.NotFound(w, r)
	}
}

func get(mux *http.ServeMux, url string) *httptest.ResponseRecorder {
	recorder := httptest.NewRecorder()
	mux.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, url, nil))
	return recorder
}

func TestAssetRoute(t *testing.T) {
	cases := []struct {
		name       string
		url        string
		wantStatus int
		wantURL    string
	}{
		{
			name:       "exact platform match",
			url:        "/releases/stable/latest/asset?asset=thock&os=macos&arch=aarch64",
			wantStatus: http.StatusOK,
			wantURL:    "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
		},
		{
			name:       "the client's built-in zed asset name is an alias",
			url:        "/releases/stable/latest/asset?asset=zed&os=linux&arch=x86_64",
			wantStatus: http.StatusOK,
			wantURL:    "https://storage.googleapis.com/thock-releases/dist/v1.16.0/thock-linux-x86_64.tar.gz",
		},
		{
			name:       "telemetry query params are ignored",
			url:        "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64&metrics_id=m&system_id=s&is_staff=false",
			wantStatus: http.StatusOK,
			wantURL:    "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
		},
		{
			name:       "pinned version matching the manifest",
			url:        "/releases/stable/1.16.0/asset?asset=zed&os=macos&arch=aarch64",
			wantStatus: http.StatusOK,
			wantURL:    "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
		},
		{
			name:       "unpublished version",
			url:        "/releases/stable/1.2.3/asset?asset=zed&os=macos&arch=aarch64",
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "unbuilt os and arch",
			url:        "/releases/stable/latest/asset?asset=zed&os=windows&arch=x86_64",
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "asset thock does not ship",
			url:        "/releases/stable/latest/asset?asset=zed-remote-server&os=macos&arch=aarch64",
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "unknown channel",
			url:        "/releases/beta/latest/asset?asset=zed&os=macos&arch=aarch64",
			wantStatus: http.StatusNotFound,
		},
		{
			name:       "channel name that is really a path",
			url:        "/releases/..%2Fdist%2Fv1.16.0%2Fmanifest/latest/asset?asset=zed&os=macos&arch=aarch64",
			wantStatus: http.StatusNotFound,
		},
	}

	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			_, mux := newTestServer(t, stableBucket(t))
			response := get(mux, testCase.url)

			if response.Code != testCase.wantStatus {
				t.Fatalf("got %d, want %d (body: %s)", response.Code, testCase.wantStatus, response.Body)
			}
			if response.Code == http.StatusOK {
				var r release
				if err := json.Unmarshal(response.Body.Bytes(), &r); err != nil {
					t.Fatalf("response is not valid JSON: %v", err)
				}
				if r.Version != "1.16.0" || r.URL != testCase.wantURL {
					t.Fatalf("got %+v, want version 1.16.0 and url %s", r, testCase.wantURL)
				}
				if cacheControl := response.Header().Get("Cache-Control"); cacheControl != "public, max-age=60" {
					t.Fatalf("got Cache-Control %q", cacheControl)
				}
			} else {
				assertReadableMiss(t, response)
			}
		})
	}
}

// Every miss carries a readable sentence, and nothing reaches a 500.
func assertReadableMiss(t *testing.T, response *httptest.ResponseRecorder) {
	t.Helper()
	if response.Code >= http.StatusInternalServerError && response.Code != http.StatusServiceUnavailable {
		t.Fatalf("got %d; a broken bucket is a 503 and a miss is a 404, never a 500", response.Code)
	}
	body := strings.TrimSpace(response.Body.String())
	if body == "" || strings.Contains(body, "goroutine") {
		t.Fatalf("error body should be a sentence, got %q", body)
	}
}

func TestReleaseNotesRedirect(t *testing.T) {
	_, mux := newTestServer(t, stableBucket(t))
	response := get(mux, "/releases/stable/1.16.0")

	if response.Code != http.StatusFound {
		t.Fatalf("got %d, want 302 (body: %s)", response.Code, response.Body)
	}
	if location := response.Header().Get("Location"); location != "https://github.com/DiegoTavares/thock/releases/tag/v1.16.0" {
		t.Fatalf("got Location %q", location)
	}
}

func TestMalformedManifestIs503(t *testing.T) {
	_, mux := newTestServer(t, func(w http.ResponseWriter, _ *http.Request) {
		if _, err := w.Write([]byte("not json {")); err != nil {
			t.Errorf("writing body: %v", err)
		}
	})
	response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64")

	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503 (body: %s)", response.Code, response.Body)
	}
	assertReadableMiss(t, response)
}

func TestUnreachableBucketIs503(t *testing.T) {
	bucketServer := httptest.NewServer(http.NotFoundHandler())
	bucketServer.Close()
	s := newServer(bucketServer.URL)

	response := get(s.routes(), "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64")

	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503 (body: %s)", response.Code, response.Body)
	}
	assertReadableMiss(t, response)
}

func TestStaleManifestOutlivesABucketBlip(t *testing.T) {
	var failing atomic.Bool
	stable := stableBucket(t)
	s, mux := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		if failing.Load() {
			http.Error(w, "bucket on fire", http.StatusInternalServerError)
			return
		}
		stable(w, r)
	})

	currentTime := time.Unix(1_700_000_000, 0)
	s.now = func() time.Time { return currentTime }

	if response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64"); response.Code != http.StatusOK {
		t.Fatalf("priming fetch failed: %d %s", response.Code, response.Body)
	}

	failing.Store(true)
	currentTime = currentTime.Add(10 * time.Minute)

	response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64")
	if response.Code != http.StatusOK {
		t.Fatalf("stale manifest should still serve: %d %s", response.Code, response.Body)
	}
	var r release
	if err := json.Unmarshal(response.Body.Bytes(), &r); err != nil || r.Version != "1.16.0" {
		t.Fatalf("stale response wrong: %s (err %v)", response.Body, err)
	}
}

func TestChannelDeletionIsAuthoritative(t *testing.T) {
	var deleted atomic.Bool
	stable := stableBucket(t)
	s, mux := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		if deleted.Load() {
			http.NotFound(w, r)
			return
		}
		stable(w, r)
	})

	currentTime := time.Unix(1_700_000_000, 0)
	s.now = func() time.Time { return currentTime }

	if response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64"); response.Code != http.StatusOK {
		t.Fatalf("priming fetch failed: %d %s", response.Code, response.Body)
	}

	deleted.Store(true)
	currentTime = currentTime.Add(10 * time.Minute)

	response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64")
	if response.Code != http.StatusNotFound {
		t.Fatalf("a deleted channel should 404 even with a cached manifest: %d %s", response.Code, response.Body)
	}
}

func TestManifestFetchIsCached(t *testing.T) {
	var fetches atomic.Int32
	stable := stableBucket(t)
	_, mux := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		fetches.Add(1)
		stable(w, r)
	})

	for i := 0; i < 3; i++ {
		if response := get(mux, "/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64"); response.Code != http.StatusOK {
			t.Fatalf("fetch %d failed: %d", i, response.Code)
		}
	}
	if fetches.Load() != 1 {
		t.Fatalf("bucket fetched %d times within the TTL, want 1", fetches.Load())
	}
}

func TestHealthzAndUnknownPaths(t *testing.T) {
	_, mux := newTestServer(t, stableBucket(t))

	if response := get(mux, "/healthz"); response.Code != http.StatusOK {
		t.Fatalf("healthz: got %d", response.Code)
	}
	for _, url := range []string{"/", "/account", "/docs/getting-started", "/releases", "/releases/stable"} {
		response := get(mux, url)
		if response.Code != http.StatusNotFound {
			t.Fatalf("%s: got %d, want 404", url, response.Code)
		}
		assertReadableMiss(t, response)
	}
}
