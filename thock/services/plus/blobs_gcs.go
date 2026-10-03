package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
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
type gcsBlobStore struct {
	bucket  gcsBucket
	signURL func(object string, options *storage.SignedURLOptions) (string, error)
}

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

func (b *gcsBlobStore) downloadURL(_ context.Context, vaultID, blobID string, expires time.Time) (string, error) {
	url, err := b.signURL(gcsObjectName(vaultID, blobID), &storage.SignedURLOptions{
		Scheme:  storage.SigningSchemeV4,
		Method:  http.MethodGet,
		Expires: expires,
	})
	if err != nil {
		return "", fmt.Errorf("signing the download: %w", err)
	}
	return url, nil
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
	err := b.bucket.delete(ctx, gcsObjectName(vaultID, blobID))
	if err == nil || errors.Is(err, storage.ErrObjectNotExist) {
		return nil
	}
	return fmt.Errorf("deleting the object: %w", err)
}
