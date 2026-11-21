package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sync"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

type fakeBucket struct {
	objects map[string]int64
	deleted []string
}

func (f *fakeBucket) attrs(_ context.Context, object string) (*storage.ObjectAttrs, error) {
	size, ok := f.objects[object]
	if !ok {
		return nil, storage.ErrObjectNotExist
	}
	return &storage.ObjectAttrs{Name: object, Size: size}, nil
}

func (f *fakeBucket) delete(_ context.Context, object string) error {
	if _, ok := f.objects[object]; !ok {
		return storage.ErrObjectNotExist
	}
	delete(f.objects, object)
	f.deleted = append(f.deleted, object)
	return nil
}

func fakeGCSStore(objects map[string]int64) (*gcsBlobStore, *fakeBucket, *[]storage.SignedURLOptions) {
	bucket := &fakeBucket{objects: objects}
	var signed []storage.SignedURLOptions
	store := &gcsBlobStore{
		bucket: bucket,
		now:    time.Now,
		signURL: func(object string, options *storage.SignedURLOptions) (string, error) {
			signed = append(signed, *options)
			return "https://storage.example/" + object + "?signature=x", nil
		},
	}
	return store, bucket, &signed
}

func TestGCSStoreNamesObjectsByVaultAndBlobAndSignsV4(t *testing.T) {
	store, _, signed := fakeGCSStore(nil)
	expires := time.Date(2026, 10, 2, 15, 0, 0, 0, time.UTC)

	upload, err := store.uploadURL(context.Background(), "v1", "b1", 42, expires)
	if err != nil {
		t.Fatal(err)
	}
	if upload.URL != "https://storage.example/v1/b1?signature=x" || upload.Method != http.MethodPut {
		t.Fatalf("unexpected upload %+v", upload)
	}
	if upload.Headers["Content-Type"] != "application/octet-stream" || !upload.Expires.Equal(expires) {
		t.Fatalf("unexpected upload %+v", upload)
	}
	download, until, err := store.downloadURL(context.Background(), "v1", "b1", expires)
	if err != nil {
		t.Fatal(err)
	}
	if download != "https://storage.example/v1/b1?signature=x" || !until.Equal(expires) {
		t.Fatalf("unexpected download %q", download)
	}
	if len(*signed) != 2 {
		t.Fatalf("expected two signatures, got %d", len(*signed))
	}
	for _, options := range *signed {
		if options.Scheme != storage.SigningSchemeV4 || !options.Expires.Equal(expires) {
			t.Fatalf("unexpected signing options %+v", options)
		}
	}
	if (*signed)[0].Method != http.MethodPut || (*signed)[0].ContentType != "application/octet-stream" {
		t.Fatalf("the upload must be signed as a PUT of an octet stream: %+v", (*signed)[0])
	}
	if (*signed)[1].Method != http.MethodGet || (*signed)[1].ContentType != "" {
		t.Fatalf("the download must be signed as a plain GET: %+v", (*signed)[1])
	}
}

func TestGCSStoreStatAndDeleteMapNotFound(t *testing.T) {
	store, bucket, _ := fakeGCSStore(map[string]int64{"v1/b1": 2242})

	size, err := store.stat(context.Background(), "v1", "b1")
	if err != nil || size != 2242 {
		t.Fatalf("stat: %d, %v", size, err)
	}
	if _, err := store.stat(context.Background(), "v1", "missing"); !errors.Is(err, errNotFound) {
		t.Fatalf("a missing object must read as errNotFound, got %v", err)
	}
	if err := store.delete(context.Background(), "v1", "b1"); err != nil {
		t.Fatal(err)
	}
	if err := store.delete(context.Background(), "v1", "b1"); err != nil {
		t.Fatalf("deleting twice must be quiet, got %v", err)
	}
	if len(bucket.deleted) != 1 || bucket.deleted[0] != "v1/b1" {
		t.Fatalf("unexpected deletions %v", bucket.deleted)
	}
}

func TestGCSStoreRejectsAnEmptyBucketName(t *testing.T) {
	if _, err := newGCSBlobStore(context.Background(), ""); err == nil {
		t.Fatal("an empty bucket name must be refused")
	}
}

// TestGCSStoreRoundTripsThroughARealBucket runs only with THOCK_GCS_BUCKET
// set and application-default credentials that can sign: it uploads through
// a signed PUT, reads the size back, downloads through a signed GET and
// deletes.
func TestGCSStoreRoundTripsThroughARealBucket(t *testing.T) {
	bucket := os.Getenv("THOCK_GCS_BUCKET")
	if bucket == "" {
		t.Skip("THOCK_GCS_BUCKET is not set")
	}
	ctx := context.Background()
	store, err := newGCSBlobStore(ctx, bucket)
	if err != nil {
		t.Fatal(err)
	}
	random := make([]byte, 8)
	if _, err := rand.Read(random); err != nil {
		t.Fatal(err)
	}
	vaultID, blobID := "test-"+hex.EncodeToString(random), hex.EncodeToString(random)
	body := []byte("TVS1 round trip through a real bucket")
	expires := time.Now().Add(5 * time.Minute)

	upload, err := store.uploadURL(ctx, vaultID, blobID, int64(len(body)), expires)
	if err != nil {
		t.Fatal(err)
	}
	request, err := http.NewRequestWithContext(ctx, upload.Method, upload.URL, bytes.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	for name, value := range upload.Headers {
		request.Header.Set(name, value)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	responseBody, _ := io.ReadAll(response.Body)
	response.Body.Close()
	if response.StatusCode/100 != 2 {
		t.Fatalf("upload answered %d: %s", response.StatusCode, responseBody)
	}
	t.Cleanup(func() {
		if err := store.delete(ctx, vaultID, blobID); err != nil {
			t.Logf("cleanup: %v", err)
		}
	})

	size, err := store.stat(ctx, vaultID, blobID)
	if err != nil || size != int64(len(body)) {
		t.Fatalf("stat after upload: %d, %v", size, err)
	}
	download, _, err := store.downloadURL(ctx, vaultID, blobID, expires)
	if err != nil {
		t.Fatal(err)
	}
	response, err = http.Get(download)
	if err != nil {
		t.Fatal(err)
	}
	downloaded, _ := io.ReadAll(response.Body)
	response.Body.Close()
	if response.StatusCode != http.StatusOK || !bytes.Equal(downloaded, body) {
		t.Fatalf("download answered %d with %q", response.StatusCode, downloaded)
	}
	if err := store.delete(ctx, vaultID, blobID); err != nil {
		t.Fatal(err)
	}
	if _, err := store.stat(ctx, vaultID, blobID); !errors.Is(err, errNotFound) {
		t.Fatalf("the object must be gone after delete, got %v", err)
	}
}

func TestGCSStoreHandsOutADownloadURLAgainWhileItHasLifeLeft(t *testing.T) {
	store, bucket, signed := fakeGCSStore(map[string]int64{"v1/b1": 10})
	now := time.Date(2026, 11, 21, 12, 0, 0, 0, time.UTC)
	store.now = func() time.Time { return now }
	ctx := context.Background()

	first, until, err := store.downloadURL(ctx, "v1", "b1", now.Add(signedURLLifetime))
	if err != nil {
		t.Fatal(err)
	}
	// A listing repeated a few minutes later signs nothing and tells the
	// truth about when the URL it repeats runs out.
	now = now.Add(4 * time.Minute)
	again, againUntil, err := store.downloadURL(ctx, "v1", "b1", now.Add(signedURLLifetime))
	if err != nil {
		t.Fatal(err)
	}
	if again != first || !againUntil.Equal(until) || len(*signed) != 1 {
		t.Fatalf("expected the kept URL until %v, got until %v after %d signatures", until, againUntil, len(*signed))
	}
	if _, _, err := store.downloadURL(ctx, "v1", "other", now.Add(signedURLLifetime)); err != nil || len(*signed) != 2 {
		t.Fatalf("another blob needs its own signature: %d, %v", len(*signed), err)
	}

	// With less than the floor left it is signed afresh.
	now = now.Add(2 * time.Minute)
	_, fresh, err := store.downloadURL(ctx, "v1", "b1", now.Add(signedURLLifetime))
	if err != nil {
		t.Fatal(err)
	}
	if len(*signed) != 3 || !fresh.Equal(now.Add(signedURLLifetime)) {
		t.Fatalf("expected a fresh signature, got %d until %v", len(*signed), fresh)
	}

	// A deleted blob's URL is not handed out again.
	if err := store.delete(ctx, "v1", "b1"); err != nil {
		t.Fatal(err)
	}
	bucket.objects["v1/b1"] = 10
	if _, _, err := store.downloadURL(ctx, "v1", "b1", now.Add(signedURLLifetime)); err != nil || len(*signed) != 4 {
		t.Fatalf("expected a signature after the delete: %d, %v", len(*signed), err)
	}
}

// countingBlobs signs slowly enough to overlap and records how many
// signatures ran at once.
type countingBlobs struct {
	blobStore
	mu       sync.Mutex
	running  int
	peak     int
	signed   int
	failBlob string
}

func (c *countingBlobs) downloadURL(_ context.Context, _, blobID string, expires time.Time) (string, time.Time, error) {
	c.mu.Lock()
	c.running++
	c.signed++
	c.peak = max(c.peak, c.running)
	c.mu.Unlock()
	time.Sleep(time.Millisecond)
	c.mu.Lock()
	c.running--
	c.mu.Unlock()
	if blobID == c.failBlob {
		return "", time.Time{}, errors.New("signing is down")
	}
	return "https://storage.example/" + blobID, expires, nil
}

func TestAListingSignsItsDownloadsSideBySideAndInOrder(t *testing.T) {
	rows := make([]fileRow, 200)
	for i := range rows {
		rows[i] = fileRow{Path: fmt.Sprintf("notes/%03d.md", i), BlobID: fmt.Sprintf("blob%03d", i), Deleted: i%50 == 7}
	}
	blobs := &countingBlobs{}
	s := &server{blobs: blobs}
	expires := time.Date(2026, 11, 21, 12, 15, 0, 0, time.UTC)

	downloads, err := s.signDownloads(context.Background(), "v1", rows, expires)
	if err != nil {
		t.Fatal(err)
	}
	for i, row := range rows {
		want := "https://storage.example/" + row.BlobID
		if row.Deleted {
			want = ""
		}
		if downloads[i].url != want {
			t.Fatalf("row %d got %q, want %q", i, downloads[i].url, want)
		}
	}
	if blobs.signed != 196 {
		t.Fatalf("a tombstone needs no signature: %d signed", blobs.signed)
	}
	if blobs.peak < 2 || blobs.peak > signConcurrency {
		t.Fatalf("expected between 2 and %d signatures at once, saw %d", signConcurrency, blobs.peak)
	}

	// One failure fails the listing and stops the rest from being signed.
	failing := &countingBlobs{failBlob: "blob003"}
	s = &server{blobs: failing}
	if _, err := s.signDownloads(context.Background(), "v1", rows, expires); err == nil {
		t.Fatal("a signature that fails must fail the listing")
	}
	if failing.signed > 3+2*signConcurrency {
		t.Fatalf("signing went on after the failure: %d signed", failing.signed)
	}

	// A caller that has gone is not signed for.
	gone, cancel := context.WithCancel(context.Background())
	cancel()
	idle := &countingBlobs{}
	s = &server{blobs: idle}
	if _, err := s.signDownloads(gone, "v1", rows, expires); err == nil || idle.signed != 0 {
		t.Fatalf("expected no signatures for a caller that left: %d, %v", idle.signed, err)
	}
}
