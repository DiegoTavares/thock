# Thock V20 — Rollout runbook (infrastructure & ship)

**Status:** Executed — infrastructure live, both workflows rehearsed; first stable build not yet
shipped (2026-09-05)
**Owner:** Diego · **Companion:** `v20-auto-update.md` (the design; §10–§15 are what this runbook executes)

The V20 code is done and on this branch: the client delta, the Cloud Run service
(`thock/services/releases/`), the manifest writer, and both workflows. All tests pass
(`go test ./...`, `python3 write_manifest_test.py`, `cargo test -p auto_update`). What remains is
standing up the GCP side and shipping the first build. This file exists because that work moves to a
different machine: a fresh session can execute it top to bottom without the original conversation.

## Decisions already made (do not re-litigate)

| Decision | Value |
|---|---|
| GCP project | **`thock-505921`** — Diego's existing project, deliberately deviating from the spec's dedicated-project recommendation (his call, 2026-09-04). |
| Region | **`us-central1`** (supports Cloud Run domain mappings). |
| Domain | **`updates.thethock.com`** — Diego owns `thethock.com`, DNS is in **Cloud DNS** in GCP. |
| Bucket | **`gs://thock-releases`** — bucket names are global, so if taken fall back to `thock-releases-505921` and use that name *everywhere*: the `BUCKET` env var on the service, the `GCP_RELEASES_BUCKET` repo variable, and nowhere else (manifest URLs embed it). |
| WIF condition | The design spec's `ref_type == tag` binding would lock out the promote workflow, which dispatches from a branch. Bind to `repository == 'DiegoTavares/thock' && (ref_type == 'tag' \|\| ref == 'refs/heads/main')` instead. |

## Execution record (2026-09-05)

Done: §1–§6 in full, and §7.1's pipeline rehearsal in both directions. `updates.thethock.com` serves
all four routes over a Google-issued cert. A throwaway `v1.14.0` tag ran `publish-updates` end to end
against a scratch bucket — artifacts uploaded with immutable cache headers, manifest archived beside
them, channel flipped with `no-store` — and the promote workflow then created a `preview` channel
byte-identical to the archived manifest. Both ran under WIF with no long-lived credential. Tag,
draft release, and scratch bucket were deleted afterwards.

Not done: §7.2's local client rehearsal, and §8's first stable build.

Three things bit that this runbook did not predict, all now folded into §0 and §3:

1. **The default compute service account had no roles**, so the first `run deploy` 403'd on its own
   source zip.
2. **Domain Restricted Sharing** at the org blocked every public grant.
3. **`GET /healthz` never reaches the container** — Cloud Run's frontend answers that exact path
   itself. Neighbouring paths (`/healthz/`, `/nope`) reach the app fine, so it presents as a routing
   bug in your own code. The service now serves `/health`; the tell was the missing
   `x-cloud-trace-context` header on the 404.

And one that blocked the pipeline rather than the infrastructure: **the Linux release build had never
succeeded.** `script/bundle-linux` exports `CC` but not `CXX`, so webrtc-sys' C++ compiled with g++,
which rejects webrtc's `Network()` declaration; the `-Wno-changes-meaning` webrtc-sys passes only
exists in GCC 14+ and is ignored on the ubuntu-22.04 runner. `CXX: clang++` on the Linux Bundle step
fixes it, and `publish`/`publish-updates` had never run before that because they need both platforms.

## 0. Machine prerequisites

```sh
brew install --cask google-cloud-sdk   # or: brew install google-cloud-sdk
# If `gcloud` is "command not found" after install, it's a PATH problem, not a
# missing install — add the SDK bin dir (the brew caveats print it, e.g.
# /opt/homebrew/share/google-cloud-sdk/bin) to PATH.
gcloud auth login
gcloud config set project thock-505921

# The brew SDK ships without the beta component; §4's domain mapping is the only
# step that needs it, and it cannot prompt for the install non-interactively.
gcloud components install beta

gh auth status   # needs repo admin on DiegoTavares/thock for `gh variable set`
```

Two grants the fresh machine will not have, both of which fail in ways that look like something else:

```sh
# Source-based `gcloud run deploy` builds as the default compute SA. On a project
# that has never run Cloud Build, that account has *zero* roles, and §3 dies with
# a 403 reading the source zip it just uploaded — which reads like a bucket
# permissions bug rather than a missing role.
gcloud projects add-iam-policy-binding thock-505921 \
  --member="serviceAccount:$(gcloud projects describe thock-505921 --format='value(projectNumber)')-compute@developer.gserviceaccount.com" \
  --role=roles/cloudbuild.builds.builder --condition=None
```

**Domain Restricted Sharing.** `thock-505921` sits under an organization that enforces
`constraints/iam.allowedPolicyMemberDomains`, which refuses `allUsers` outright — so §1's public read
*and* §3's `--allow-unauthenticated` both fail with `HTTPError 412: One or more users named in the
policy do not belong to a permitted customer`. The narrow fix does not exist: that constraint rejects
`allUsers` and `allAuthenticatedUsers` as list values. Override it for this project only, which
leaves the org default untouched everywhere else (needs `roles/orgpolicy.policyAdmin` at the org):

```sh
cat > /tmp/drs.yaml <<'EOF'
name: projects/327645078899/policies/iam.allowedPolicyMemberDomains
spec:
  rules:
  - allowAll: true
EOF
gcloud services enable orgpolicy.googleapis.com
gcloud org-policies set-policy /tmp/drs.yaml
# Propagation takes a couple of minutes; retry the §1 grant until it sticks.
```

`storage.publicAccessPrevention` and `run.allowedIngress` were already permissive on this project —
DRS was the only blocker. Check all three before assuming which one is biting.

Enable the APIs once:

```sh
gcloud services enable run.googleapis.com cloudbuild.googleapis.com \
  artifactregistry.googleapis.com iamcredentials.googleapis.com \
  sts.googleapis.com dns.googleapis.com
```

## 1. Bucket

```sh
gcloud storage buckets create gs://thock-releases \
  --location=us-central1 --uniform-bucket-level-access
gcloud storage buckets update gs://thock-releases --versioning
gcloud storage buckets add-iam-policy-binding gs://thock-releases \
  --member=allUsers --role=roles/storage.objectViewer
```

Object versioning is the manifest-rollback safety net; `allUsers` viewer is required — the client
downloads artifact URLs with no auth header. **Nothing but shippable artifacts goes in this bucket.**

## 2. Service accounts

```sh
gcloud iam service-accounts create thock-releases-api \
  --display-name="Thock release index runtime"
gcloud iam service-accounts create thock-release-publisher \
  --display-name="Thock release publisher (GitHub CI)"

# CI writes artifacts and flips channels — objectAdmin on this one bucket, nothing else.
gcloud storage buckets add-iam-policy-binding gs://thock-releases \
  --member="serviceAccount:thock-release-publisher@thock-505921.iam.gserviceaccount.com" \
  --role=roles/storage.objectAdmin

# The service reads manifests over public HTTPS and needs no role at all; give it
# objectViewer anyway so the day manifests go private is a one-line change.
gcloud storage buckets add-iam-policy-binding gs://thock-releases \
  --member="serviceAccount:thock-releases-api@thock-505921.iam.gserviceaccount.com" \
  --role=roles/storage.objectViewer
```

## 3. Cloud Run service

From the repo root (buildpacks compile the Go source; no Dockerfile):

```sh
gcloud run deploy thock-releases-api \
  --source thock/services/releases \
  --region us-central1 \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 3 \
  --service-account thock-releases-api@thock-505921.iam.gserviceaccount.com \
  --set-env-vars BUCKET=thock-releases
```

Smoke it immediately with a hand-written manifest (adjust `version` to the current
`crates/zed/Cargo.toml` version + a patch bump so a test build would see it as an update):

```sh
cat > /tmp/stable.json <<'EOF'
{"version":"1.14.1","notes_url":"https://github.com/DiegoTavares/thock/releases",
 "assets":[{"asset":"thock","os":"macos","arch":"aarch64",
 "url":"https://storage.googleapis.com/thock-releases/dist/v1.14.1/Thock-aarch64.dmg","sha256":"test"}]}
EOF
gcloud storage cp /tmp/stable.json gs://thock-releases/channels/stable.json --cache-control="no-store"

URL="$(gcloud run services describe thock-releases-api --region us-central1 --format='value(status.url)')"
curl -s "$URL/health"                                                         # ok
curl -s "$URL/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64"   # {"version":"1.14.1",...}
curl -si "$URL/releases/stable/1.14.1" | head -3                              # 302 → notes_url
curl -s "$URL/releases/stable/latest/asset?asset=zed&os=windows&arch=x86_64"  # 404, a sentence
```

## 4. Domain

```sh
# May require one-time domain verification first: gcloud domains verify thethock.com
gcloud beta run domain-mappings create \
  --service thock-releases-api --domain updates.thethock.com --region us-central1

# The describe output names the DNS record to create (CNAME → ghs.googlehosted.com.)
gcloud beta run domain-mappings describe --domain updates.thethock.com --region us-central1

gcloud dns managed-zones list   # find the thethock.com zone name
gcloud dns record-sets create updates.thethock.com. \
  --zone=<ZONE_NAME> --type=CNAME --ttl=300 --rrdatas=ghs.googlehosted.com.
```

TLS provisioning takes minutes to an hour after the record resolves. Verify with the same curls
against `https://updates.thethock.com`. **Do not ship any stable build before this works** — the
host is baked into shipped builds forever (design spec, decision 5).

## 5. Workload Identity Federation

```sh
gcloud iam workload-identity-pools create github \
  --location=global --display-name="GitHub Actions"

gcloud iam workload-identity-pools providers create-oidc thock \
  --location=global --workload-identity-pool=github \
  --display-name="DiegoTavares/thock" \
  --issuer-uri="https://token.actions.githubusercontent.com" \
  --attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository,attribute.ref_type=assertion.ref_type,attribute.ref=assertion.ref" \
  --attribute-condition="assertion.repository == 'DiegoTavares/thock' && (assertion.ref_type == 'tag' || assertion.ref == 'refs/heads/main')"

PROJECT_NUMBER="$(gcloud projects describe thock-505921 --format='value(projectNumber)')"
gcloud iam service-accounts add-iam-policy-binding \
  thock-release-publisher@thock-505921.iam.gserviceaccount.com \
  --role=roles/iam.workloadIdentityUser \
  --member="principalSet://iam.googleapis.com/projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/github/attribute.repository/DiegoTavares/thock"
```

The tag-or-main condition is the control: tag pushes run `publish-updates`, `workflow_dispatch` of
the promote workflow runs from `main`. Anyone who can push a `v*` tag can ship code to every
install — protect tags like a deploy key.

## 6. GitHub repository variables

Variables, not secrets — none are sensitive, and both workflows skip while they're unset:

```sh
gh variable set GCP_RELEASES_BUCKET --repo DiegoTavares/thock \
  --body "thock-releases"
gh variable set GCP_WORKLOAD_IDENTITY_PROVIDER --repo DiegoTavares/thock \
  --body "projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/github/providers/thock"
gh variable set GCP_RELEASE_SERVICE_ACCOUNT --repo DiegoTavares/thock \
  --body "thock-release-publisher@thock-505921.iam.gserviceaccount.com"
```

## 7. Rehearse before the first real tag

Per the design spec §14 — the first real tag must not be the first run of either workflow:

1. **Pipeline rehearsal:** temporarily point `GCP_RELEASES_BUCKET` at a scratch bucket, push a
   throwaway tag matching the crate version (delete the tag and draft release after). The whole
   `publish-updates` job runs against real GCP and flips a channel nobody reads. Then run the
   promote workflow against the same scratch bucket. Point the variable back.
2. **Client rehearsal, no cloud:** the local loop in §14 of the design spec — bundle at the current
   version, install; bump `crates/zed/Cargo.toml`, bundle again; serve bundle + hand-written
   manifest via `python3 -m http.server`; launch the installed app with
   `ZED_SERVER_URL=http://localhost:8000`; run `thock: check for updates`. This is what catches
   install-path regressions. Repeat on Linux if a machine is handy.
3. Delete the hand-written smoke manifest (or overwrite it via the first real publish) before
   telling any tester a build is live: `channels/stable.json` must only ever hold manifests that
   real artifacts back.

## 8. Ship

1. Bump `crates/zed/Cargo.toml` version, commit, tag `v<version>`, push the tag. The workflow
   builds, drafts the GitHub release, uploads to `dist/`, and flips `channels/stable.json`.
2. Every existing install predates the updater — they were built with `ZED_UPDATE_EXPLANATION`
   compiled in. **This first stable build is a manual install for every current tester, and the
   last one.** macOS treats the stable bundle as a new app (identifier `dev.zed.Zed`); vault,
   settings, and panel state all survive (keyed off `paths::APP_NAME`).
3. The build after that is the one that proves the feature: push the next tag, wait an hour,
   watch a tester's app update itself.

## 9. After it's live (bookkeeping)

- Update `thock/VISION.md` §12: add the V20 roadmap entry under Milestone 4 as _(shipped)_ —
  and **republish the VISION.md artifact at its existing URL** (never a new one).
- Flip `v20-auto-update.md` status to shipped and resolve its §16 open questions with what was
  actually done (region, domain, notes URL target).
- Consider notarization (design spec §13) before widening beyond friendly testers.
