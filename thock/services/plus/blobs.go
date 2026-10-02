package main

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// Where the encrypted snapshots live. The API process only hands out signed
// URLs and checks that an object exists; the bytes never pass through the
// main routes. The local store is the default and is complete: with it,
// `go run .` is a whole sync server on one machine. The Cloud Storage store
// in blobs_gcs.go is what production runs (BLOB_STORE=gcs).
type blobStore interface {
	// uploadURL signs a PUT for exactly size bytes.
	uploadURL(ctx context.Context, vaultID, blobID string, size int64, expires time.Time) (signedURL, error)
	downloadURL(ctx context.Context, vaultID, blobID string, expires time.Time) (string, error)
	// stat returns the object's size, or errNotFound.
	stat(ctx context.Context, vaultID, blobID string) (int64, error)
	delete(ctx context.Context, vaultID, blobID string) error
}

type signedURL struct {
	URL     string            `json:"url"`
	Method  string            `json:"method"`
	Headers map[string]string `json:"headers"`
	Expires time.Time         `json:"expires_at"`
}

// localBlobStore keeps blobs as files under dir and serves them through the
// API process itself at /v1/vault/blobs/{token}. The token carries the
// operation, object and expiry, HMAC-signed with a per-process key, so a URL
// is as opaque to the clients as a bucket's would be.
type localBlobStore struct {
	dir       string
	key       []byte
	publicURL func() string
	now       func() time.Time
}

func newLocalBlobStore(dir string, key []byte, publicURL func() string) (*localBlobStore, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("creating the blob directory: %w", err)
	}
	return &localBlobStore{dir: dir, key: key, publicURL: publicURL, now: time.Now}, nil
}

type blobToken struct {
	Op      string `json:"op"`
	Vault   string `json:"v"`
	Blob    string `json:"b"`
	Size    int64  `json:"s,omitempty"`
	Expires int64  `json:"e"`
}

func (b *localBlobStore) sign(token blobToken) (string, error) {
	raw, err := json.Marshal(token)
	if err != nil {
		return "", err
	}
	body := base64.RawURLEncoding.EncodeToString(raw)
	mac := hmac.New(sha256.New, b.key)
	mac.Write([]byte(body))
	return body + "." + base64.RawURLEncoding.EncodeToString(mac.Sum(nil)), nil
}

func (b *localBlobStore) verify(signed string) (blobToken, error) {
	body, signature, ok := strings.Cut(signed, ".")
	if !ok {
		return blobToken{}, errors.New("malformed token")
	}
	mac := hmac.New(sha256.New, b.key)
	mac.Write([]byte(body))
	want := mac.Sum(nil)
	got, err := base64.RawURLEncoding.DecodeString(signature)
	if err != nil || !hmac.Equal(got, want) {
		return blobToken{}, errors.New("bad signature")
	}
	raw, err := base64.RawURLEncoding.DecodeString(body)
	if err != nil {
		return blobToken{}, err
	}
	var token blobToken
	if err := json.Unmarshal(raw, &token); err != nil {
		return blobToken{}, err
	}
	if b.now().Unix() > token.Expires {
		return blobToken{}, errExpired
	}
	return token, nil
}

func (b *localBlobStore) path(vaultID, blobID string) string {
	return filepath.Join(b.dir, vaultID, blobID)
}

func (b *localBlobStore) uploadURL(_ context.Context, vaultID, blobID string, size int64, expires time.Time) (signedURL, error) {
	token, err := b.sign(blobToken{Op: "put", Vault: vaultID, Blob: blobID, Size: size, Expires: expires.Unix()})
	if err != nil {
		return signedURL{}, err
	}
	return signedURL{
		URL:     b.publicURL() + "/v1/vault/blobs/" + token,
		Method:  http.MethodPut,
		Headers: map[string]string{"Content-Type": "application/octet-stream"},
		Expires: expires,
	}, nil
}

func (b *localBlobStore) downloadURL(_ context.Context, vaultID, blobID string, expires time.Time) (string, error) {
	token, err := b.sign(blobToken{Op: "get", Vault: vaultID, Blob: blobID, Expires: expires.Unix()})
	if err != nil {
		return "", err
	}
	return b.publicURL() + "/v1/vault/blobs/" + token, nil
}

func (b *localBlobStore) stat(_ context.Context, vaultID, blobID string) (int64, error) {
	info, err := os.Stat(b.path(vaultID, blobID))
	if errors.Is(err, os.ErrNotExist) {
		return 0, errNotFound
	}
	if err != nil {
		return 0, err
	}
	return info.Size(), nil
}

func (b *localBlobStore) delete(_ context.Context, vaultID, blobID string) error {
	err := os.Remove(b.path(vaultID, blobID))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}

// serve handles PUT and GET on /v1/vault/blobs/{token}. Bodies are written
// to a temporary file and renamed into place so a dropped upload never
// leaves a half object behind that a commit could mistake for the real one.
func (b *localBlobStore) serve(w http.ResponseWriter, r *http.Request) {
	token, err := b.verify(r.PathValue("token"))
	if errors.Is(err, errExpired) {
		writeErrorCode(w, http.StatusGone, "expired", "That upload link has expired. Start the upload again.")
		return
	}
	if err != nil {
		writeErrorCode(w, http.StatusUnauthorized, "unauthorized", "That isn't a valid blob link.")
		return
	}
	switch {
	case r.Method == http.MethodPut && token.Op == "put":
		if r.ContentLength != token.Size {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "The upload's length doesn't match what was announced.")
			return
		}
		dir := filepath.Join(b.dir, token.Vault)
		if err := os.MkdirAll(dir, 0o700); err != nil {
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't store the upload.")
			return
		}
		temp, err := os.CreateTemp(dir, "."+token.Blob+".*")
		if err != nil {
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't store the upload.")
			return
		}
		written, err := io.Copy(temp, http.MaxBytesReader(w, r.Body, token.Size))
		closeErr := temp.Close()
		if err != nil || closeErr != nil || written != token.Size {
			if removeErr := os.Remove(temp.Name()); removeErr != nil {
				logf("warning: removing a failed upload: %v", removeErr)
			}
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "The upload didn't arrive whole.")
			return
		}
		if err := os.Rename(temp.Name(), b.path(token.Vault, token.Blob)); err != nil {
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't store the upload.")
			return
		}
		w.WriteHeader(http.StatusOK)
	case r.Method == http.MethodGet && token.Op == "get":
		file, err := os.Open(b.path(token.Vault, token.Blob))
		if errors.Is(err, os.ErrNotExist) {
			writeErrorCode(w, http.StatusNotFound, "not_found", "That blob is gone.")
			return
		}
		if err != nil {
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the blob.")
			return
		}
		defer file.Close()
		info, err := file.Stat()
		if err != nil {
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the blob.")
			return
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("Content-Length", strconv.FormatInt(info.Size(), 10))
		w.Header().Set("Cache-Control", "private, no-store")
		w.WriteHeader(http.StatusOK)
		if _, err := io.Copy(w, file); err != nil {
			logf("warning: streaming a blob: %v", err)
		}
	default:
		writeErrorCode(w, http.StatusMethodNotAllowed, "bad_request", "That link doesn't allow this method.")
	}
}
