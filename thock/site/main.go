// The Thock website (spec: thock/specs/v21-site-hosting-and-download-gate.md).
//
// A static file server plus one route. Everything under public/ is compiled
// into the binary and served as-is, so Cloud Run's Go buildpack can build it
// from source with no Dockerfile — the same shape as the release index in
// thock/services/releases. The download gate lives entirely in the page
// (download.html + gate.json); the only server-side logic is POST /waitlist,
// which records a signup in Firestore and logs it so a Cloud Logging alert can
// email Diego. Standard library only: Firestore is reached over REST with the
// Cloud Run service account's token, so there is nothing to vendor.
package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"embed"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net/http"
	"os"
	"regexp"
	"strings"
	"time"
)

//go:embed public
var publicFiles embed.FS

// The canonical host. Requests for www.<host> are redirected here so the site
// has one address.
const canonicalHost = "thethock.com"

const defaultProject = "thock-505921"

// Loose on purpose: the point is to reject garbage and typos, not to validate
// deliverability. Anything that passes still has to be a real inbox to ever
// receive an invite.
var emailPattern = regexp.MustCompile(`^[^@\s]+@[^@\s]+\.[^@\s]+$`)

const maxSignupBody = 4 << 10

var errAlreadyOnList = errors.New("already on the list")

// waitlist writes signups to the Firestore collection `waitlist`, one document
// per address, keyed by a hash of the address so a repeat signup is a no-op
// rather than a duplicate.
type waitlist struct {
	// e.g. https://firestore.googleapis.com/v1/projects/<p>/databases/(default)/documents
	documentsBase string
	token         func(context.Context) (string, error)
	client        *http.Client
	now           func() time.Time
	// Structured log sink; Cloud Run turns one JSON object per line into a log
	// entry, and the alert policy matches on jsonPayload.event.
	logs io.Writer
}

func newWaitlist(project string) *waitlist {
	return &waitlist{
		documentsBase: fmt.Sprintf("https://firestore.googleapis.com/v1/projects/%s/databases/(default)/documents", project),
		token:         metadataToken,
		client:        &http.Client{Timeout: 10 * time.Second},
		now:           time.Now,
		logs:          os.Stdout,
	}
}

// The access token of the service the container runs as, from the Cloud Run
// metadata server. Not cached: signups are rare and the metadata server is
// local and fast.
func metadataToken(ctx context.Context) (string, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet,
		"http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token", nil)
	if err != nil {
		return "", err
	}
	request.Header.Set("Metadata-Flavor", "Google")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return "", err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return "", fmt.Errorf("metadata server: %s", response.Status)
	}
	var body struct {
		AccessToken string `json:"access_token"`
	}
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		return "", err
	}
	return body.AccessToken, nil
}

func normalizeEmail(raw string) (string, bool) {
	email := strings.ToLower(strings.TrimSpace(raw))
	if email == "" || len(email) > 254 || !emailPattern.MatchString(email) {
		return "", false
	}
	return email, true
}

func documentID(email string) string {
	sum := sha256.Sum256([]byte(email))
	return hex.EncodeToString(sum[:16])
}

// record creates waitlist/<hash>. Firestore answers 409 when the document
// exists, which is how a repeat signup is told apart from a new one.
func (w *waitlist) record(ctx context.Context, email string) error {
	token, err := w.token(ctx)
	if err != nil {
		return fmt.Errorf("token: %w", err)
	}
	document := map[string]any{
		"fields": map[string]any{
			"email":     map[string]string{"stringValue": email},
			"joined_at": map[string]string{"timestampValue": w.now().UTC().Format(time.RFC3339Nano)},
		},
	}
	body, err := json.Marshal(document)
	if err != nil {
		return err
	}
	url := w.documentsBase + "/waitlist?documentId=" + documentID(email)
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("Content-Type", "application/json")
	response, err := w.client.Do(request)
	if err != nil {
		return fmt.Errorf("firestore: %w", err)
	}
	defer response.Body.Close()
	switch response.StatusCode {
	case http.StatusOK:
		return nil
	case http.StatusConflict:
		return errAlreadyOnList
	default:
		detail, _ := io.ReadAll(io.LimitReader(response.Body, 512))
		return fmt.Errorf("firestore: %s: %s", response.Status, strings.TrimSpace(string(detail)))
	}
}

func (w *waitlist) logEvent(severity, event, message string, fields map[string]any) {
	entry := map[string]any{"severity": severity, "event": event, "message": message}
	for key, value := range fields {
		entry[key] = value
	}
	if err := json.NewEncoder(w.logs).Encode(entry); err != nil {
		log.Printf("log %s: %v", event, err)
	}
}

func writeJSON(w http.ResponseWriter, status int, body map[string]any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(body); err != nil {
		log.Printf("write response: %v", err)
	}
}

// POST /waitlist with {"email": "..."}. The page shows `error` verbatim, so
// every failure is a sentence.
func (w *waitlist) handleSignup(writer http.ResponseWriter, request *http.Request) {
	var body struct {
		Email string `json:"email"`
	}
	if err := json.NewDecoder(io.LimitReader(request.Body, maxSignupBody)).Decode(&body); err != nil {
		writeJSON(writer, http.StatusBadRequest, map[string]any{"error": "That request didn't carry an email address."})
		return
	}
	email, ok := normalizeEmail(body.Email)
	if !ok {
		writeJSON(writer, http.StatusBadRequest, map[string]any{"error": "That doesn't look like an email address."})
		return
	}
	err := w.record(request.Context(), email)
	switch {
	case err == nil:
		w.logEvent("NOTICE", "waitlist_signup", "waitlist signup: "+email, map[string]any{"email": email})
		writeJSON(writer, http.StatusOK, map[string]any{"ok": true})
	case errors.Is(err, errAlreadyOnList):
		w.logEvent("INFO", "waitlist_repeat", "waitlist repeat signup: "+email, map[string]any{"email": email})
		writeJSON(writer, http.StatusOK, map[string]any{"ok": true})
	default:
		w.logEvent("ERROR", "waitlist_error", "waitlist signup failed: "+err.Error(), map[string]any{"email": email})
		writeJSON(writer, http.StatusServiceUnavailable, map[string]any{"error": "The list isn't reachable right now. Mind trying again in a minute?"})
	}
}

func handler(files fs.FS, signups *waitlist) http.Handler {
	fileServer := http.FileServer(http.FS(files))
	mux := http.NewServeMux()
	mux.HandleFunc("/waitlist", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.Header().Set("Allow", http.MethodPost)
			http.Error(w, "The waitlist takes POST requests only.", http.StatusMethodNotAllowed)
			return
		}
		signups.handleSignup(w, r)
	})
	mux.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		if _, err := w.Write([]byte("ok")); err != nil {
			log.Printf("health: %v", err)
		}
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		// Clean URLs: /download serves download.html. Anything else is exactly a
		// file under public/ or a 404.
		if r.URL.Path == "/download" {
			r.URL.Path = "/download.html"
		}
		w.Header().Set("Cache-Control", "public, max-age=300")
		w.Header().Set("X-Content-Type-Options", "nosniff")
		fileServer.ServeHTTP(w, r)
	})
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.EqualFold(r.Host, "www."+canonicalHost) {
			target := "https://" + canonicalHost + r.URL.RequestURI()
			http.Redirect(w, r, target, http.StatusMovedPermanently)
			return
		}
		mux.ServeHTTP(w, r)
	})
}

func main() {
	files, err := fs.Sub(publicFiles, "public")
	if err != nil {
		log.Fatalf("embedded public/ directory: %v", err)
	}
	project := os.Getenv("FIRESTORE_PROJECT")
	if project == "" {
		project = defaultProject
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	log.Printf("thock site listening on :%s (waitlist → firestore project %s)", port, project)
	if err := http.ListenAndServe(":"+port, handler(files, newWaitlist(project))); err != nil {
		log.Fatal(err)
	}
}
