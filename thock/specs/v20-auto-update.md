# Thock V20 — Updates that arrive on their own

**Status:** Implemented — client, service, and pipeline on `main`; GCP infrastructure pending (2026-09-04)
**Owner:** Diego · **Date:** 2026-09-04
**Companion docs:** `../VISION.md` (§4.3 Human-in-the-loop, §12 Milestone 4),
`v12-de-zed-ification.md` (which disabled the updater, and why), `../site/README.md` (the download
page that hands out the first install)

---

## 1. Summary

Thock ships as a DMG and a tarball that a tester downloads once and then keeps forever. Every fix
after that is an email with a link. V20 makes the app update itself.

The client half already exists and works: `crates/auto_update/` is Zed's updater, fully open source,
already compiled into the fork and initialized at `crates/zed/src/main.rs:663`. It polls hourly,
downloads with progress, and installs in place. What it talks to — the endpoint that answers "what's
the newest build for this OS and arch?" — is **not** open source. It lives in Zed Cloud
(`cloud.zed.dev`, resolved at `crates/http_client/src/http_client.rs:295`); `collab`, the server that
*is* in this repo, has no release routes, and the `zed-industries` org publishes no `zed.dev` or
`cloud` repository.

There is nothing to port, because the contract is one GET returning two fields:

```json
{ "version": "1.16.0", "url": "https://…/Thock-aarch64.dmg" }
```

So V20 is: **a ~100-line Cloud Run service** that answers that question from a manifest in GCS, a
release pipeline that publishes artifacts and the manifest, and a short list of Zed-shaped constants
in the client that have to learn the word "Thock".

The invariant that shapes the design: **the tag is the human act.** Pushing `v1.16.0` builds it,
uploads it, and flips the channel in one run — nothing else does, and nothing reaches a user's
machine that a human didn't tag. A separate promote workflow exists only to point the channel back
at an older build, so the way out is as short as the way in.

## 2. What already exists

| Piece | Where | State |
|---|---|---|
| Poll loop, hourly (15 min on nightly) | `crates/auto_update/src/auto_update.rs:455` | Works; gated on the `auto_update` setting and `ReleaseChannel::poll_for_updates()` |
| Release lookup | `auto_update.rs:642` (`get_release_asset`) | Works; needs a host that answers |
| Download with progress | `auto_update.rs:1037` | Works |
| Install (macOS DMG, Linux tarball) | `auto_update.rs:1090` / `:1158` | Works, but hardcodes Zed's names — §8 |
| Restart prompt / status UI | `crates/auto_update_ui/`, `crates/title_bar/src/update_version.rs` | Works; copy reads "Update to Version: X", no Zed branding |
| Bundling | `script/bundle-mac`, `script/bundle-linux` | Produces `Thock-aarch64.dmg`, `thock-linux-x86_64.tar.gz` |
| Release CI | `.github/workflows/release.yml` | Builds both on `v*` tags, publishes a **draft** GitHub Release |
| The endpoint | — | Does not exist. This spec. |

Three things currently switch the updater off, all deliberately (V12):

1. `script/bundle-mac:60` exports `ZED_UPDATE_EXPLANATION`, which is read with `option_env!` at
   compile time (`auto_update.rs:279`) and permanently disables polling *and* the menu action.
2. `crates/zed/RELEASE_CHANNEL` is `dev`, and `ReleaseChannel::poll_for_updates()` is false for Dev
   (`crates/release_channel/src/lib.rs:187`).
3. *(found during implementation)* `assets/settings/default.json` shipped `"auto_update": false`.
   V20 flips it back to `true` — safe because dev-channel builds don't poll regardless (item 2),
   and required because a shipped stable build honors the setting.

## 3. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Why a service at all | **Content negotiation.** The client's path carries channel and version but not OS or arch — those arrive as query params (`?asset=&os=&arch=`). A static JSON object in a bucket cannot vary on them, so something has to run. Cloud Run is the smallest thing that does. |
| 2 | Runtime | **Go, one file, no Dockerfile** — `gcloud run deploy --source .` uses Google's buildpacks for Go directly. A Rust service would mean a multi-stage Dockerfile and minutes of build for 100 lines of routing. (If one-language-everywhere matters more than deploy ergonomics, axum + distroless is the swap; nothing else in the spec changes.) **Standard library only** — the manifest is read over HTTPS from the public bucket rather than through `cloud.google.com/go/storage`, so there is no `go.sum` to keep current and nothing to vendor. That SDK is the one dependency to add if manifests ever stop being public. |
| 3 | Where it lives | **`thock/services/releases/`** — in this repo, next to the spec and the CI that feeds it, outside the Cargo workspace (`Cargo.toml` lists members explicitly, so a non-Rust directory is inert). |
| 4 | Source of truth | **A manifest object in GCS**, written by CI, read by the service. Not the GitHub Releases API: the repo is private, so that path needs a token in the request path and couples uptime to GitHub. |
| 5 | The URL is forever | The update host is **baked into shipped builds and can only be changed by an update**, which is exactly what breaks when the host is wrong. So: **a domain we own** (`updates.thethock.com` throughout this spec), mapped onto Cloud Run — never the raw `*.run.app` URL. |
| 6 | How the client learns the host | **Repoint `server_url`** (`assets/settings/default.json:2701`) rather than adding a Thock-only setting and threading it through `http_client`. One line, and unknown hosts already fall through unrewritten (`http_client.rs:295`). Consequence in §8. |
| 7 | Asset naming | The client asks for `asset=zed` (`auto_update.rs:725`). **The service aliases `zed` → `thock`** instead of patching that literal — one upstream line we don't have to own. |
| 8 | Promotion | **The tag ships it.** A `v*` push uploads the artifacts, archives a per-version manifest beside them, and writes `channels/stable.json` in the same run. Pushing a version tag is already the deliberate act; requiring a second one for every release makes the common path the annoying path, and annoying paths get skipped. A `workflow_dispatch` **promote** workflow exists for the other direction: re-point the channel at any archived version, one input, no rebuild. Automatic forward, manual backward. |
| 9 | Channel of shipped builds | **`stable`.** `crates/zed/RELEASE_CHANNEL` stays `dev` so local `cargo run` builds never poll; the release workflow exports `ZED_RELEASE_CHANNEL=stable` and the bundle scripts honor a pre-set value. *(Implementation note: release binaries compile the channel in via `include_str!` — the env var is only read in debug builds — so a pre-set value is written through to `crates/zed/RELEASE_CHANNEL` by the bundle scripts before building. CI checkouts are throwaway; local builds without the env var never touch the file.)* |
| 10 | Platform scope | **macOS aarch64 and Linux x86_64** — what `release.yml` builds. Every other `(os, arch)` gets a clean 404, which an automatic check swallows silently (`auto_update.rs:515`). |
| 11 | Integrity | **TLS + the code signature**, no client-side checksum in V20. The client has no verification hook and adding one is an upstream patch; `sha256` goes in the manifest for the download page and for humans. |

## 4. Goals & success criteria

**Primary:** a tester running 1.15.0 is running 1.16.0 within an hour of the `v1.16.0` tag being
pushed, having done nothing but restart when asked — and nothing in the flow says "Zed".

**Definition of done:**

1. `GET https://updates.thethock.com/releases/stable/latest/asset?asset=zed&os=macos&arch=aarch64`
   returns `{version, url}` for the promoted build, and 404s for an `(os, arch)` we don't ship.
2. Pushing a `v*` tag uploads both artifacts, archives their manifest, and flips
   `channels/stable.json` — with no long-lived credential in GitHub (Workload Identity Federation).
3. The promote workflow re-points the channel at any archived version and touches nothing else: the
   rollback is one dispatch and rebuilds nothing.
4. A locally built stable-channel bundle pointed at a local manifest via `ZED_SERVER_URL` downloads,
   installs, and relaunches itself on both macOS and Linux.
5. `thock: check for updates` in the palette reports a real error when the host is unreachable, and
   an automatic check stays silent.
6. The app that comes out the other side is the same app: same vault, same settings, same panel
   state.

## 5. Non-goals

- **Windows.** `release.yml` doesn't build it; `auto_update_helper` is ready when it does.
- **x86_64 macOS.** Same reason. Adding either is a manifest entry and a CI job, not a design change.
- **Staged / percentage rollouts.** The client only identifies itself when telemetry is on, and
  Thock ships it off (`assets/settings/default.json:1562`), so `system_id` is always absent and there
  is nothing to hash a bucket from. Rollout control is: promote, or don't.
- **Delta updates.** A 200 MB download an hour after a promote, once, is fine.
- **Downgrades.** The client updates strictly forward (`check_if_fetched_version_is_newer_non_nightly`,
  `auto_update.rs:921`). Rolling the manifest back stops *new* upgrades; it does not reach back into
  installs that already took one.
- **A separate preview channel** as a thing users are on. The plumbing is channel-shaped and free,
  and §14 leans on a `preview` channel for rehearsal, but nobody is shipped from it until Thock has
  more than a handful of testers.

## 6. The wire contract

What the client sends (`get_release_asset`, `auto_update.rs:642`):

```
GET /releases/{channel}/{version}/asset
      ?asset=zed&os=macos&arch=aarch64
      [&metrics_id=…&system_id=…&is_staff=…]   # only when telemetry is on; Thock: never
```

- `{channel}` is `ReleaseChannel::dev_name()` — `stable` for shipped builds.
- `{version}` is the literal `latest` for the app. An exact version appears only on the
  remote-server path (`auto_update.rs:621`), which Thock does not ship; serve it if it matches the
  manifest, 404 otherwise.

What it expects back — `ReleaseAsset`, `auto_update.rs:187`, two fields, extras ignored:

```json
{ "version": "1.16.0",
  "url": "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg" }
```

Then it GETs `url` directly (`auto_update.rs:1045`) — so that URL must be publicly fetchable with no
auth header, and should send `Content-Length` or the progress bar has nothing to divide by.

Error behavior matters more than it looks: a non-2xx is surfaced verbatim to the user on a **manual**
check and swallowed on an **automatic** one (`auto_update.rs:515`). So a 404 body is a message a
human will eventually read — make it a sentence, not a stack trace.

One more route, for free: `release_notes_url` (`auto_update.rs:339`) builds
`{server_url}/releases/{channel}/{version}` — no `/asset` suffix. Serving a 302 to the GitHub release
page there makes **View Release Notes** work instead of 404ing.

## 7. The manifest

`gs://thock-releases/channels/stable.json`:

```json
{
  "version": "1.16.0",
  "released_at": "2026-09-10T12:00:00Z",
  "notes_url": "https://github.com/DiegoTavares/thock/releases/tag/v1.16.0",
  "assets": [
    {
      "asset": "thock", "os": "macos", "arch": "aarch64",
      "url": "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
      "sha256": "…"
    },
    {
      "asset": "thock", "os": "linux", "arch": "x86_64",
      "url": "https://storage.googleapis.com/thock-releases/dist/v1.16.0/thock-linux-x86_64.tar.gz",
      "sha256": "…"
    }
  ]
}
```

`version` must equal the `version` in `crates/zed/Cargo.toml` (currently `1.14.0`) with no `v`
prefix — the client parses it as semver and compares it against its own. CI guards that the tag, the
Cargo version, and the manifest agree; a mismatch is the failure mode where either nobody updates or
everybody updates forever.

Writing this one object is the whole promotion. It is the only mutable thing in the system.

## 8. The client delta

Every line here is outside `crates/thock/` and `thock/`, so each one is rebase surface and belongs in
the PR body's upstream-touch-points section.

**Turning the updater back on**

1. `script/bundle-mac:58-60` — delete the `ZED_UPDATE_EXPLANATION` export. Thock-owned lines added by
   V12; with them present nothing else in this spec has any effect.
2. `script/bundle-mac:51-54` and `script/bundle-linux:48` — read `ZED_RELEASE_CHANNEL` from the
   environment when it's already set, falling back to `crates/zed/RELEASE_CHANNEL`. Lets the release
   workflow build `stable` while local builds stay `dev`.
3. `crates/zed/Cargo.toml` — `[package.metadata.bundle-stable]` still says `name = "Zed"` with Zed's
   icons; only `bundle-dev` was rebranded (line 306). Copy the Thock name and icons across. **Keep
   `identifier = "dev.zed.Zed"`** — it has to match `ReleaseChannel::app_id`
   (`crates/release_channel/src/lib.rs:214`), which is also the Wayland/X11 app ID.
4. `assets/settings/default.json:2701` — `"server_url": "https://updates.thethock.com"`. This also
   repoints every `zed_urls` helper (account, docs, terms) at our host; those surfaces are hidden by
   V12, and §9 gives them a 404 with a sentence rather than a hang.

**Two hardcoded Zed names that break the install outright**

5. `install_release_macos` (`auto_update.rs:1158`) mounts the DMG with `-mountroot <tmp>` and then
   looks in `<tmp>/Zed` — the subdirectory is the *volume* name, and Thock's DMG is created with
   `-volname Thock` (`script/bundle-mac:263`). Without this fix every macOS update fails at rsync,
   after a full download.
6. `install_release_linux` (`auto_update.rs:1090`) expects `zed{suffix}.app` containing
   `libexec/zed-editor`; `script/bundle-linux:149-153` produces `thock.app` with
   `libexec/thock-editor`. (With channel `stable` the suffix is empty, so only the name is wrong.)

Both get a `// Thock:` comment, matching how the fork annotates its other upstream edits.

**Deliberately not changed:** `asset=zed` at `auto_update.rs:725` (decision 7), and the two
collab-only strings at `title_bar.rs:1191/1197` that mention Zed — they render only in the
collaboration flow, which Thock doesn't show.

## 9. The service

`thock/services/releases/main.go` — stateless, one manifest read per cache miss.

| Route | Behavior |
|---|---|
| `GET /releases/{channel}/{version}/asset` | Resolve `(asset, os, arch)` against the channel manifest. `200 {version,url}`, or `404` with a plain-language body. `asset=zed` is accepted as an alias for `thock`. `{version}` may be `latest` or an exact match. |
| `GET /releases/{channel}/{version}` | `302` to the manifest's `notes_url`. |
| `GET /healthz` | `200 ok`. |
| anything else | `404`, plain text. Repointing `server_url` sends Zed's account and docs links here too (§8 item 4); they get an answer rather than a hang. |

Three behaviors the routes don't show:

- **A 60 s in-process cache per channel.** Clients poll hourly, so a promote is live within a minute
  and the bucket sees a handful of reads a day.
- **Stale beats absent.** If a refresh fails and a previous manifest is in hand, serve it and log a
  warning — a promoted release should outlive a blip in front of the bucket.
- **A missing channel is a 404, a broken bucket is a 503, and neither is a 500.** The client shows
  this text verbatim to whoever ran a manual check (`auto_update.rs:515`), so every body is a
  sentence: *"The Thock release index is temporarily unavailable. Try again shortly."*

```go
// sketch, not the implementation
func (s *server) asset(w http.ResponseWriter, r *http.Request) {
    channel, version := r.PathValue("channel"), r.PathValue("version") // net/http 1.22 routing
    m, err := s.manifest(r.Context(), channel) // bucket read, 60s in-process TTL
    if err != nil { writeManifestError(w, channel, err); return } // 404 or 503, never 500
    if version != "latest" && version != m.Version {
        http.Error(w, "no such version", http.StatusNotFound); return
    }
    q := r.URL.Query()
    name := q.Get("asset")
    if name == "zed" { name = "thock" } // the client's built-in asset name
    for _, a := range m.Assets {
        if a.Asset == name && a.OS == q.Get("os") && a.Arch == q.Get("arch") {
            w.Header().Set("Cache-Control", "public, max-age=60")
            json.NewEncoder(w).Encode(release{Version: m.Version, URL: a.URL})
            return
        }
    }
    http.Error(w, "Thock does not publish a build for this platform yet.", http.StatusNotFound)
}
```

Cache the manifest in memory for 60 s. Clients poll hourly; a promote is visible within a minute;
GCS gets a handful of reads a day.

## 10. GCP topology

**Project.** A dedicated project (`thock-releases`). This bucket is a code-execution channel into
every install — it should not share an IAM blast radius with the marketing site or anything
experimental.

**Bucket** `gs://thock-releases`, uniform bucket-level access, **object versioning on** (a bad
manifest is recoverable, and every promote is auditable):

```
dist/v1.16.0/Thock-aarch64.dmg           Cache-Control: public, max-age=31536000, immutable
dist/v1.16.0/thock-linux-x86_64.tar.gz   (same)
dist/v1.16.0/manifest.json               (same) — the archived manifest a rollback copies from
channels/stable.json                     Cache-Control: no-store — the live channel
```

`allUsers: roles/storage.objectViewer` on the bucket. Uniform access means that's bucket-wide,
manifests included — neither is secret, and the artifacts must be anonymously fetchable anyway
(§6). The rule that follows: **nothing but shippable artifacts goes in this bucket.**

**Service.**

```sh
gcloud run deploy thock-releases-api \
  --source thock/services/releases \
  --region us-central1 \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 3 \
  --service-account thock-releases-api@thock-releases.iam.gserviceaccount.com \
  --set-env-vars BUCKET=thock-releases
```

The service reads manifests over public HTTPS (decision 2), so its service account needs no bucket
role at all — give it one anyway, with nothing else attached, so the day manifests go private is a
one-line change rather than an IAM investigation.

**Domain.**

```sh
gcloud beta run domain-mappings create \
  --service thock-releases-api --domain updates.thethock.com --region us-central1
```

Domain mapping isn't offered in every region; where it isn't, the fallback is a global external
Application Load Balancer with a serverless NEG (more moving parts, also gives Cloud CDN in front of
the JSON), or a Firebase Hosting rewrite onto the service (cheapest, one `firebase.json`). Pick one
before shipping the first build — decision 5 means this is not revisitable afterwards.

**Cost.** One small JSON response per install per hour, cold-starting from zero. Comfortably inside
the free tier at beta scale; artifact egress from GCS is the only line item that grows.

## 11. Release pipeline

Two workflows and three repository **variables** — not secrets; none of these are sensitive, and
keeping them out of the YAML means the files survive the project being recreated:

| Variable | Example |
|---|---|
| `GCP_RELEASES_BUCKET` | `thock-releases` |
| `GCP_WORKLOAD_IDENTITY_PROVIDER` | `projects/123456789/locations/global/workloadIdentityPools/github/providers/thock` |
| `GCP_RELEASE_SERVICE_ACCOUNT` | `thock-release-publisher@thock-releases.iam.gserviceaccount.com` |

Both workflows are guarded on `vars.GCP_RELEASES_BUCKET != ''`, so tags keep building and drafting
releases before any of the GCP side exists. The job reports as **skipped**, not green — a publish
step that silently succeeds without publishing is the one outcome worth engineering against.

### Publish — a new job in `release.yml`, after `publish`

```yaml
  publish-updates:
    name: Publish update channel
    if: startsWith(github.ref, 'refs/tags/v') && vars.GCP_RELEASES_BUCKET != ''
    needs: [publish]
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write          # Workload Identity Federation; no key in secrets
    env:
      BUCKET: ${{ vars.GCP_RELEASES_BUCKET }}
    steps:
      - uses: actions/checkout@v4

      # A tag that disagrees with the crate version either updates nobody or
      # updates everybody forever, depending on which way it disagrees.
      - name: Verify the tag matches the crate version
        run: |
          crate="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/zed/Cargo.toml | head -1)"
          [ "v${crate}" = "${GITHUB_REF_NAME}" ] || {
            echo "tag ${GITHUB_REF_NAME} does not match crates/zed/Cargo.toml ${crate}" >&2
            exit 1
          }

      - uses: actions/download-artifact@v4
        with: { path: artifacts, merge-multiple: true }

      - uses: google-github-actions/auth@v2
        with:
          workload_identity_provider: ${{ vars.GCP_WORKLOAD_IDENTITY_PROVIDER }}
          service_account: ${{ vars.GCP_RELEASE_SERVICE_ACCOUNT }}
      - uses: google-github-actions/setup-gcloud@v2

      - name: Build the manifest
        run: |
          thock/services/releases/write-manifest "${GITHUB_REF_NAME}" artifacts > manifest.json
          cat manifest.json

      # Immutable, and first: no manifest may name a URL that doesn't resolve yet.
      - name: Upload artifacts
        run: |
          gcloud storage cp artifacts/* manifest.json \
            "gs://${BUCKET}/dist/${GITHUB_REF_NAME}/" \
            --cache-control="public, max-age=31536000, immutable"

      # The one write that changes what an installed app sees.
      - name: Flip the stable channel
        run: |
          gcloud storage cp \
            "gs://${BUCKET}/dist/${GITHUB_REF_NAME}/manifest.json" \
            "gs://${BUCKET}/channels/stable.json" \
            --cache-control="no-store"
          echo "Thock ${GITHUB_REF_NAME} is live on the stable channel." >> "$GITHUB_STEP_SUMMARY"
```

The ordering is the safety story: artifacts are immutable and land first, the manifest is archived
beside them, and the channel object is written last. Every step before that last one is invisible to
installed apps, so a run that dies halfway leaves nothing half-shipped.

### Promote — `.github/workflows/promote-release.yml`, `workflow_dispatch`

Inputs: `version` (`v1.15.0`) and `channel` (default `stable`). The job asserts that
`dist/<version>/manifest.json` exists, copies it onto `channels/<channel>.json`, and writes what it
did to the step summary. No build, no artifacts, no recomputation — the manifest it promotes is
byte-for-byte the one that shipped with that tag.

This is the rollback. It is also how a `beta` channel gets pointed at a build for testing, and how a
channel is repaired if a manifest is ever written by hand.

### The manifest writer — `thock/services/releases/write-manifest`

Takes the tag and the artifact directory, maps each filename to its `(os, arch)`, hashes it, and
emits §7's JSON on stdout. Filenames are its contract with `script/bundle-*`:

| Artifact | os / arch |
|---|---|
| `Thock-aarch64.dmg` | `macos` / `aarch64` |
| `Thock-x86_64.dmg` | `macos` / `x86_64` |
| `thock-linux-x86_64.tar.gz` | `linux` / `x86_64` |
| `thock-linux-aarch64.tar.gz` | `linux` / `aarch64` |

An artifact whose name matches nothing is a **hard error**, not a skipped line. Silently dropping one
is how half the fleet gets an update and the other half doesn't.

### What deliberately stays manual

The GitHub Release is still created as a **draft** (`release.yml:122`), and this job doesn't change
that. The two are now decoupled on purpose: the draft is for humans and the download page, the
manifest is for installed apps. If they should move together, publishing the release is one flag on
the step that already exists.

The download page (`thock/site/index.html`) links the same `dist/` URLs, so promoting a build and
updating the page draw on the same facts.

## 12. Channel and identity migration

The stable-channel bundle carries `identifier = "dev.zed.Zed"` where today's dev bundle carries
`dev.zed.Zed-Dev`. Consequences, all one-time:

- **Nothing in the vault or in local state moves.** `config_dir()`/`data_dir()` key off
  `paths::APP_NAME` (`crates/paths/src/paths.rs:18`, `"Thock"`), not the channel. Settings, the kvp
  store, panel state and onboarding flags all survive.
- **macOS treats it as a different application** for LaunchServices, the `zed://` URL scheme and the
  keychain. Existing installs must be replaced by hand, once.
- **Existing installs can never auto-update to it** — they were built with
  `ZED_UPDATE_EXPLANATION` compiled in. V20's first build is a manual install for every current
  tester, and that's the last one.

## 13. Signing, and the trust boundary

Auto-update turns the bucket, the domain, and the CI identity into a code-execution channel — and
since the tag flips the channel (decision 8), **the tag is the control**. Push access to tags on this
repo is push access to every install. The mitigations are the ones already in the spec — WIF bound to
`repository == DiegoTavares/thock` *and* `ref_type == tag`, `objectAdmin` on one bucket and nothing
else, object versioning, a dedicated project, a rollback that is one dispatch — plus one that isn't:

**Notarize the macOS build.** `release.yml` currently ships an ad-hoc signature (the comment at
`release.yml:50` lists the secrets that would change that). Beyond the first-launch Gatekeeper
warning, a Developer ID signature is the thing that makes a compromised bucket insufficient on its
own: the replacement app still has to be signed by a key that isn't in GCP.

Worth verifying rather than assuming during implementation: the quarantine attribute is applied by
the *downloading* application, so a DMG fetched by the updater's own HTTP client should arrive
without one, and rsyncing an ad-hoc-signed app over a running ad-hoc-signed app should work. If that
holds, unsigned auto-update functions — it just leaves the trust boundary where it is. Treat it as an
observation to confirm on a real machine (§14), not a reason to skip signing.

## 14. Testing

**Service** — table-driven Go tests against an `httptest` stand-in for the bucket: exact match, the
`zed` alias, `latest` vs pinned vs unpublished version, unbuilt os/arch, unknown channel, a channel
name that is really a path, the telemetry query params the client may or may not send, a malformed
manifest, an unreachable bucket, and the stale-manifest fallback. Every miss carries a readable body;
nothing reaches a 500.

**Manifest writer** — a fixture directory of empty files named like real artifacts, asserting the
`(os, arch)` mapping, the hashes, and that an unrecognized filename fails the run.

**Client, without any cloud** — this is the loop that actually catches the install bugs in §8:

1. `script/bundle-mac` (or `bundle-linux`) at the current version, install it.
2. Bump `crates/zed/Cargo.toml`, bundle again, serve the second bundle plus a hand-written manifest
   from `python3 -m http.server`.
3. Launch the installed app with `ZED_SERVER_URL=http://localhost:8000` — the env override at
   `crates/client/src/client.rs:63` reaches `ClientSettings` and falls through `build_zed_cloud_url`
   unrewritten, so no rebuild is needed to retarget the updater.
4. `thock: check for updates` → download, install, restart prompt, relaunch on the new version, with
   the vault and panel state intact.

Repeat on Linux. The existing `FakeHttpClient` tests in `auto_update.rs:1321` cover the version
comparison and stay green for free; they do not exercise the platform install paths, which is exactly
where §8's two bugs live.

**End to end**, without the live channel ever moving — two levers, and the first real tag should not
be the first run of either:

- Point `GCP_RELEASES_BUCKET` at a scratch bucket and push a throwaway tag. The whole publish job
  runs against real GCP and flips a channel nobody reads.
- Build a bundle with `ZED_RELEASE_CHANNEL=preview` and promote a manifest onto
  `channels/preview.json`. The service serves whatever channel exists, so the client half gets
  exercised against production infrastructure while `stable` sits still.

## 15. Rollout

1. **Client plumbing** — §8, verified entirely against a local HTTP server. No GCP, no domain, fully
   revertable.
2. **Infrastructure** — bucket, service, WIF, domain. Manifest written by hand; `curl` is the test.
3. **Pipeline** — both workflows, rehearsed on a throwaway tag against a scratch bucket (§14).
4. **Ship** — first stable build, installed by hand by the tester (§12). The build after it is the
   first one that arrives on its own, and the one that proves the feature.

## 16. Risks and open questions

| Risk | Response |
|---|---|
| The update host is baked into shipped builds and unfixable without an update | Own the domain before step 4; never ship pointing at `*.run.app`. |
| A broken build reaches everyone because the tag flips the channel | Accepted, and the cost of decision 8. Object versioning and a one-dispatch rollback shorten the window, but installs that already took the bad build are only fixable by another release. If that ever stings, the gate to add back is a `needs: [approval]` environment on `publish-updates`, not a second workflow. |
| Tag push is production access | WIF is bound to `repository == DiegoTavares/thock` and `ref_type == tag`, so nothing else can mint a publishing token — but anyone who can push a `v*` tag can ship code to every install. Protect tags the way you'd protect a deploy key. |
| macOS install fails after a full download (the `-mountroot` name) | §8 item 5, and the local loop in §14 catches it before anyone sees it. |
| Repointing `server_url` breaks a Zed surface still reachable in the UI | Audit `zed_urls` callers during implementation; the service answers unknown paths with a 404 and a sentence rather than hanging. |
| Ad-hoc signing makes the first install scary and the update path unverifiable | §13. Notarization is a prerequisite for calling this shippable beyond friendly testers. |

**Open:** the Cloud Run region (match the rest of Diego's services, and check domain mapping is
offered there); whether the download page moves into the same bucket or stays wherever `thock/site`
is hosted today; whether `notes_url` should point at a public changelog on `thethock.com` rather than
a GitHub release page a tester can't see, since the repo is private.

## 17. Future work

- **Windows**, once `release.yml` builds it — `auto_update_helper` already handles the
  overwrite-on-quit dance.
- **x86_64 macOS**, a CI job and a manifest entry.
- **sha256 verification in the client**, which is a real upstream patch to `download_release` and
  worth it once there is anything to lose.
- **Staged rollouts**, which need a stable per-install identifier the client doesn't currently send
  with telemetry off.
- **A preview channel**, free in the plumbing, meaningless below a handful of testers.
