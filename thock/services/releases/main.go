// The Thock release index (spec: thock/specs/v20-auto-update.md).
//
// Answers the auto-updater's one question — "what is the newest build for this
// OS and arch?" — from a channel manifest that CI writes to a public GCS
// bucket. Stateless; the manifest object is the only mutable thing in the
// system.
//
// Error bodies are shown verbatim to whoever ran a manual update check, so
// every miss is a plain-language sentence, and nothing here returns a 500: a
// missing channel is a 404 and a broken bucket is a 503.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"regexp"
	"sync"
	"time"
)

const manifestTTL = 60 * time.Second

// A channel manifest, as written by write-manifest and archived per version
// under dist/. Extra fields (released_at, sha256) are for humans and the
// download page; the service ignores them.
type manifest struct {
	Version  string  `json:"version"`
	NotesURL string  `json:"notes_url"`
	Assets   []asset `json:"assets"`
}

type asset struct {
	Asset string `json:"asset"`
	OS    string `json:"os"`
	Arch  string `json:"arch"`
	URL   string `json:"url"`
}

// What the client deserializes (ReleaseAsset in crates/auto_update).
type release struct {
	Version string `json:"version"`
	URL     string `json:"url"`
}

var errChannelNotFound = errors.New("channel not found")

// Channel names are single path segments written by our own CI; anything
// fancier is someone probing, not a client.
var channelNamePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]*$`)

type cachedManifest struct {
	manifest  manifest
	fetchedAt time.Time
}

type server struct {
	// Base URL the channel manifests are fetched from, e.g.
	// "https://storage.googleapis.com/thock-releases".
	manifestBase string
	client       *http.Client
	now          func() time.Time
	ttl          time.Duration

	mu    sync.Mutex
	cache map[string]cachedManifest
}

func newServer(manifestBase string) *server {
	return &server{
		manifestBase: manifestBase,
		client:       &http.Client{Timeout: 10 * time.Second},
		now:          time.Now,
		ttl:          manifestTTL,
		cache:        map[string]cachedManifest{},
	}
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /releases/{channel}/{version}/asset", s.handleAsset)
	mux.HandleFunc("GET /releases/{channel}/{version}", s.handleReleaseNotes)
	// Cloud Run's frontend answers the exact path /healthz itself and never
	// forwards it to the container, so /health is the one reachable in
	// production; /healthz stays registered for local runs.
	health := func(w http.ResponseWriter, _ *http.Request) {
		fmt.Fprintln(w, "ok")
	}
	mux.HandleFunc("GET /health", health)
	mux.HandleFunc("GET /healthz", health)
	// Repointing server_url sends Zed's account and docs links here too; they
	// get an answer rather than a hang.
	mux.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "This is the Thock update service. There is nothing to browse here.", http.StatusNotFound)
	})
	return mux
}

// manifest returns the current manifest for a channel, reading through a
// per-channel in-process cache. A fetch failure with a previous manifest in
// hand serves the stale copy — a promoted release should outlive a blip in
// front of the bucket. A clean 404 from the bucket is authoritative: the
// channel does not exist, cached or not.
func (s *server) manifest(ctx context.Context, channel string) (manifest, error) {
	if !channelNamePattern.MatchString(channel) {
		return manifest{}, errChannelNotFound
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	cached, haveCached := s.cache[channel]
	if haveCached && s.now().Sub(cached.fetchedAt) < s.ttl {
		return cached.manifest, nil
	}

	fetched, err := s.fetchManifest(ctx, channel)
	switch {
	case err == nil:
		s.cache[channel] = cachedManifest{manifest: fetched, fetchedAt: s.now()}
		return fetched, nil
	case errors.Is(err, errChannelNotFound):
		delete(s.cache, channel)
		return manifest{}, err
	case haveCached:
		log.Printf("warning: serving stale %s manifest: %v", channel, err)
		return cached.manifest, nil
	default:
		return manifest{}, err
	}
}

func (s *server) fetchManifest(ctx context.Context, channel string) (manifest, error) {
	url := fmt.Sprintf("%s/channels/%s.json", s.manifestBase, channel)
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return manifest{}, err
	}
	response, err := s.client.Do(request)
	if err != nil {
		return manifest{}, err
	}
	defer response.Body.Close()

	switch {
	case response.StatusCode == http.StatusOK:
	case response.StatusCode == http.StatusNotFound:
		return manifest{}, errChannelNotFound
	default:
		return manifest{}, fmt.Errorf("manifest fetch returned %s", response.Status)
	}

	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return manifest{}, err
	}
	var m manifest
	if err := json.Unmarshal(body, &m); err != nil {
		return manifest{}, fmt.Errorf("malformed manifest: %w", err)
	}
	if m.Version == "" {
		return manifest{}, errors.New("malformed manifest: missing version")
	}
	return m, nil
}

func writeManifestError(w http.ResponseWriter, channel string, err error) {
	if errors.Is(err, errChannelNotFound) {
		http.Error(w, fmt.Sprintf("There is no %q release channel.", channel), http.StatusNotFound)
		return
	}
	log.Printf("error: manifest for %s: %v", channel, err)
	http.Error(w, "The Thock release index is temporarily unavailable. Try again shortly.", http.StatusServiceUnavailable)
}

func (s *server) handleAsset(w http.ResponseWriter, r *http.Request) {
	channel, version := r.PathValue("channel"), r.PathValue("version")
	m, err := s.manifest(r.Context(), channel)
	if err != nil {
		writeManifestError(w, channel, err)
		return
	}
	if version != "latest" && version != m.Version {
		http.Error(w, fmt.Sprintf("Version %s is not the current release on the %s channel.", version, channel), http.StatusNotFound)
		return
	}

	query := r.URL.Query()
	name := query.Get("asset")
	if name == "zed" {
		// The asset name the client has built in; aliased here rather than
		// patched there (spec decision 7).
		name = "thock"
	}
	for _, candidate := range m.Assets {
		if candidate.Asset == name && candidate.OS == query.Get("os") && candidate.Arch == query.Get("arch") {
			w.Header().Set("Cache-Control", "public, max-age=60")
			w.Header().Set("Content-Type", "application/json")
			if err := json.NewEncoder(w).Encode(release{Version: m.Version, URL: candidate.URL}); err != nil {
				log.Printf("error: writing response: %v", err)
			}
			return
		}
	}
	http.Error(w, "Thock does not publish a build for this platform yet.", http.StatusNotFound)
}

// The client's View Release Notes opens {server_url}/releases/{channel}/{version};
// send it to the release page recorded in the manifest.
func (s *server) handleReleaseNotes(w http.ResponseWriter, r *http.Request) {
	channel := r.PathValue("channel")
	m, err := s.manifest(r.Context(), channel)
	if err != nil {
		writeManifestError(w, channel, err)
		return
	}
	if m.NotesURL == "" {
		http.Error(w, "This release has no published notes yet.", http.StatusNotFound)
		return
	}
	http.Redirect(w, r, m.NotesURL, http.StatusFound)
}

func main() {
	bucket := os.Getenv("BUCKET")
	if bucket == "" {
		log.Fatal("BUCKET environment variable is required (the GCS bucket holding channel manifests)")
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	s := newServer("https://storage.googleapis.com/" + bucket)
	log.Printf("thock release index listening on :%s, manifests from %s", port, s.manifestBase)
	log.Fatal(http.ListenAndServe(":"+port, s.routes()))
}
