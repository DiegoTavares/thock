# Thock release index

The Cloud Run service behind `https://updates.thethock.com` — answers the auto-updater's
"what's the newest build for this OS and arch?" from the channel manifest CI writes to GCS.
Spec: `thock/specs/v20-auto-update.md`.

- `main.go` — the whole service; Go standard library only, no Dockerfile
  (Cloud Run buildpacks build it from source).
- `write-manifest` — emits the channel manifest from a directory of release
  artifacts; called by the `publish-updates` job in `.github/workflows/release.yml`.

## Tests

```sh
go test ./...
python3 write_manifest_test.py
```

## Local run

```sh
BUCKET=thock-releases PORT=8080 go run .
curl 'localhost:8080/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64'
```

## Deploy

```sh
gcloud run deploy thock-releases-api \
  --source thock/services/releases \
  --region us-central1 \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 3 \
  --service-account thock-releases-api@thock-releases.iam.gserviceaccount.com \
  --set-env-vars BUCKET=thock-releases
```

The full GCP topology (bucket layout, IAM, domain mapping, Workload Identity
Federation for CI) is in the spec, §10–11.
