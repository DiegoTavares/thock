package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"io"
	"net/http"
	"os"
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
	download, err := store.downloadURL(context.Background(), "v1", "b1", expires)
	if err != nil {
		t.Fatal(err)
	}
	if download != "https://storage.example/v1/b1?signature=x" {
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
	download, err := store.downloadURL(ctx, vaultID, blobID, expires)
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
