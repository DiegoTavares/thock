package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"time"

	"cloud.google.com/go/storage"
)

// gcsBlobStore keeps blobs in one Cloud Storage bucket as
// <vault id>/<blob id> and hands out V4 signed URLs, so the bytes go
// straight between the devices and the bucket. On Cloud Run the client
// signs through the IAM signBlob API with the service's own account, which
// therefore needs roles/iam.serviceAccountTokenCreator on itself (deploy.sh
// grants it); locally, application-default credentials with a private key
// sign directly.
//
// Each signature on Cloud Run is therefore a remote call. A phone that lists
// the whole vault asks for one download URL per note, so download URLs are
// kept and handed out again while they have downloadReuseFloor left: a
// listing repeated within minutes signs nothing.
type gcsBlobStore struct {
	bucket  gcsBucket
	signURL func(object string, options *storage.SignedURLOptions) (string, error)
	now     func() time.Time

	mu        sync.Mutex
	downloads map[string]cachedDownload
}

type cachedDownload struct {
	url     string
	expires time.Time
}

const (
	// A kept download URL is reused only while it still has this long to
	// live, enough for a phone to fetch a full page of notes with it.
	downloadReuseFloor = 10 * time.Minute
	// Above this many kept URLs the ones too old to hand out again are
	// dropped, and if that isn't enough, all of them: the cache saves work,
	// it holds no truth.
	maxCachedDownloads = 20000
)

// gcsBucket is the slice of *storage.BucketHandle the store uses, so tests
// can stand in for the bucket without a network.
type gcsBucket interface {
	attrs(ctx context.Context, object string) (*storage.ObjectAttrs, error)
	delete(ctx context.Context, object string) error
}

type realBucket struct {
	handle *storage.BucketHandle
}

func (b realBucket) attrs(ctx context.Context, object string) (*storage.ObjectAttrs, error) {
	return b.handle.Object(object).Attrs(ctx)
}

func (b realBucket) delete(ctx context.Context, object string) error {
	return b.handle.Object(object).Delete(ctx)
}

func newGCSBlobStore(ctx context.Context, bucketName string) (*gcsBlobStore, error) {
	if bucketName == "" {
		return nil, errors.New("BLOB_BUCKET is required with BLOB_STORE=gcs")
	}
	client, err := storage.NewClient(ctx)
	if err != nil {
		return nil, fmt.Errorf("creating the storage client: %w", err)
	}
	bucket := client.Bucket(bucketName)
	return &gcsBlobStore{
		bucket:  realBucket{handle: bucket},
		signURL: bucket.SignedURL,
		now:     time.Now,
	}, nil
}

func gcsObjectName(vaultID, blobID string) string {
	return vaultID + "/" + blobID
}

func (b *gcsBlobStore) uploadURL(_ context.Context, vaultID, blobID string, _ int64, expires time.Time) (signedURL, error) {
	// The size is enforced at commit (stat must match the declared size),
	// not on the PUT: V4 signing cannot bind Content-Length.
	url, err := b.signURL(gcsObjectName(vaultID, blobID), &storage.SignedURLOptions{
		Scheme:      storage.SigningSchemeV4,
		Method:      http.MethodPut,
		Expires:     expires,
		ContentType: "application/octet-stream",
	})
	if err != nil {
		return signedURL{}, fmt.Errorf("signing the upload: %w", err)
	}
	return signedURL{
		URL:     url,
		Method:  http.MethodPut,
		Headers: map[string]string{"Content-Type": "application/octet-stream"},
		Expires: expires,
	}, nil
}

func (b *gcsBlobStore) downloadURL(_ context.Context, vaultID, blobID string, expires time.Time) (string, time.Time, error) {
	object := gcsObjectName(vaultID, blobID)
	now := b.now()
	b.mu.Lock()
	kept, ok := b.downloads[object]
	b.mu.Unlock()
	if ok && kept.expires.Sub(now) >= downloadReuseFloor {
		return kept.url, kept.expires, nil
	}
	url, err := b.signURL(object, &storage.SignedURLOptions{
		Scheme:  storage.SigningSchemeV4,
		Method:  http.MethodGet,
		Expires: expires,
	})
	if err != nil {
		return "", time.Time{}, fmt.Errorf("signing the download: %w", err)
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.downloads == nil {
		b.downloads = map[string]cachedDownload{}
	}
	if len(b.downloads) >= maxCachedDownloads {
		for name, entry := range b.downloads {
			if entry.expires.Sub(now) < downloadReuseFloor {
				delete(b.downloads, name)
			}
		}
		if len(b.downloads) >= maxCachedDownloads {
			clear(b.downloads)
		}
	}
	b.downloads[object] = cachedDownload{url: url, expires: expires}
	return url, expires, nil
}

func (b *gcsBlobStore) stat(ctx context.Context, vaultID, blobID string) (int64, error) {
	attrs, err := b.bucket.attrs(ctx, gcsObjectName(vaultID, blobID))
	if errors.Is(err, storage.ErrObjectNotExist) {
		return 0, errNotFound
	}
	if err != nil {
		return 0, fmt.Errorf("reading the object: %w", err)
	}
	return attrs.Size, nil
}

func (b *gcsBlobStore) delete(ctx context.Context, vaultID, blobID string) error {
	b.mu.Lock()
	delete(b.downloads, gcsObjectName(vaultID, blobID))
	b.mu.Unlock()
	err := b.bucket.delete(ctx, gcsObjectName(vaultID, blobID))
	if err == nil || errors.Is(err, storage.ErrObjectNotExist) {
		return nil
	}
	return fmt.Errorf("deleting the object: %w", err)
}
