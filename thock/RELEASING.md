# Releasing and deploying Thock

Everything that ships, how it gets there, and how to take it back. Written for whoever is doing the
release, person or agent.

| What | Where it runs | Ships when | Pipeline | Undo |
|---|---|---|---|---|
| Desktop app (macOS, Linux) | users' machines, via auto-update | a `vX.Y.Z` tag is pushed | `release.yml` | `promote-release.yml` |
| Site, thethock.com | Cloud Run `thock-site` | `thock/site/**` lands on `main` | `deploy-site.yml` | redeploy a revision |
| Thock Plus backend | Cloud Run `thock-plus-api` | `thock/services/plus/**` lands on `main` | `deploy-services.yml` | redeploy a revision |
| Release index, updates.thethock.com | Cloud Run `thock-releases-api` | `thock/services/releases/**` lands on `main` | `deploy-services.yml` | redeploy a revision |
| iPhone app | TestFlight / App Store | by hand, from Xcode | none yet | expire the build |

All Cloud Run deploys authenticate with Workload Identity Federation: no keys in GitHub. The pool
accepts this repository on `main` or on a tag, nothing else (setup: `specs/v20-auto-update-rollout.md` §5).

## Rules for agents

- **An agent never pushes a `v*` tag, runs `promote-release`, runs `deploy.sh`, or uploads an iOS
  build unless the user asked for that release in this session.** Each of these reaches real users
  and none can be fully undone. Preparing the version-bump PR is fine; shipping it is the user's call.
- Merging to `main` deploys the site and the services. Treat a merge that touches `thock/site/` or
  `thock/services/` as a deploy and say so in the PR.
- After anything ships, verify it with the check listed in its section and report what you saw.

## Desktop app

1. **Bump.** Branch `release-X.Y.Z` off `main`; set `version` in `crates/zed/Cargo.toml` and let
   `Cargo.lock` follow (`cargo update -p zed --precise X.Y.Z` or a scoped `cargo check -p zed`).
   Two lines change. PR it as `Release X.Y.Z` and merge.
2. **Tag.** On the merge commit: `git tag -a vX.Y.Z -m "Thock X.Y.Z" && git push origin vX.Y.Z`.
3. **Wait.** `release.yml` first verifies the tag: it matches the crate version, the commit is on
   `main`, and the `CI` check passed on it (it waits up to 30 minutes for CI still running). Then it
   builds and notarizes the macOS bundle and builds the Linux one (about 90 minutes), drafts a GitHub
   Release, uploads to `gs://thock-releases/dist/vX.Y.Z/`, and copies the manifest to
   `channels/stable.json`. **That last copy is the moment every installed app is offered the update.**
4. **Verify.**
   ```sh
   curl -s https://storage.googleapis.com/thock-releases/channels/stable.json | jq .version
   ```
   Then install the DMG from the draft release on a Mac and open a vault.
5. **Publish the draft release** (`gh release edit vX.Y.Z --draft=false`). The manifest's
   `notes_url` points at it, and a draft is a 404 for everyone else.

A manual run of `release.yml` (`workflow_dispatch`) builds the bundles as run artifacts and
publishes nothing. Use it to test a change to the bundling scripts.

**Roll back:** run `promote-release.yml` with the last good version and `channel: stable`. It copies
that version's archived manifest back; nothing is rebuilt. Apps that already updated stay on the bad
version until a newer one ships, so follow a rollback with a fixed release.

**Secrets it uses** (repository secrets): `MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`,
`APPLE_NOTARIZATION_KEY`, `APPLE_NOTARIZATION_KEY_ID`, `APPLE_NOTARIZATION_ISSUER_ID`,
`THOCK_GOOGLE_CLIENT_ID`, `THOCK_GOOGLE_CLIENT_SECRET`. If any of the five Apple ones is missing the
bundle is ad-hoc signed and the job still passes; check the notarization lines in the log.

## Site

Merging a change under `thock/site/` runs `deploy-site.yml`: Go tests, `gcloud run deploy`, then a
smoke test of `/health`, `/`, `/download` and `/gate.json`. Infrastructure: `specs/v21-site-hosting-and-download-gate.md`.

**Roll back:**

```sh
gcloud run revisions list --service thock-site --region us-central1 --limit 5
gcloud run services update-traffic thock-site --region us-central1 --to-revisions <revision>=100
```

After a rollback the service stays pinned to that revision; the next deploy needs
`gcloud run services update-traffic thock-site --region us-central1 --to-latest` to take traffic.

## Thock Plus backend

Merging a change under `thock/services/plus/` runs the `plus` job of `deploy-services.yml`: the Go
suite (which applies every migration to a fresh Postgres), `gcloud run deploy --source`, then a
smoke test (`/health` is 200 only when the database answers; `/v1/entitlement` and `/admin/users`
must refuse an anonymous caller).

The workflow ships code only. Secrets (`DATABASE_URL`, `ADMIN_TOKEN`, the OpenRouter key), the blob
bucket and IAM are provisioned by `thock/services/plus/deploy.sh`, by hand, and each new revision
inherits them. Run `deploy.sh` to rotate a secret or change an environment variable, not to deploy.

**Migrations** run at startup, before the revision serves, and are forward-only. Write each one so
the previous revision keeps working against the new schema (add, don't rename or drop), because a
rollback moves the code back and leaves the schema where it is.

**Roll back:** as for the site, with `--service thock-plus-api`. The failed run prints the exact
command with the previous revision filled in.

**One-time setup** (until this is done the job reports *skipped*):

```sh
PROJECT=thock-505921
gcloud iam service-accounts create thock-plus-deployer --project $PROJECT
DEPLOYER=thock-plus-deployer@$PROJECT.iam.gserviceaccount.com
# The same narrow set the site deployer holds (specs/v21 §3).
gcloud projects add-iam-policy-binding $PROJECT --member serviceAccount:$DEPLOYER --role roles/cloudbuild.builds.editor --condition=None
gcloud projects add-iam-policy-binding $PROJECT --member serviceAccount:$DEPLOYER --role projects/$PROJECT/roles/thockBucketLister --condition=None
gcloud run services add-iam-policy-binding thock-plus-api --project $PROJECT --region us-central1 --member serviceAccount:$DEPLOYER --role roles/run.developer
gcloud storage buckets add-iam-policy-binding gs://run-sources-$PROJECT-us-central1 --member serviceAccount:$DEPLOYER --role roles/storage.admin
gcloud artifacts repositories add-iam-policy-binding cloud-run-source-deploy --project $PROJECT --location us-central1 --member serviceAccount:$DEPLOYER --role roles/artifactregistry.writer
for account in thock-plus-api@$PROJECT.iam.gserviceaccount.com "$(gcloud projects describe $PROJECT --format='value(projectNumber)')-compute@developer.gserviceaccount.com"; do
  gcloud iam service-accounts add-iam-policy-binding "$account" --project $PROJECT --member serviceAccount:$DEPLOYER --role roles/iam.serviceAccountUser
done
gcloud iam service-accounts add-iam-policy-binding $DEPLOYER --project $PROJECT --role roles/iam.workloadIdentityUser \
  --member "principalSet://iam.googleapis.com/projects/$(gcloud projects describe $PROJECT --format='value(projectNumber)')/locations/global/workloadIdentityPools/github/attribute.repository/DiegoTavares/thock"
gh variable set GCP_PLUS_DEPLOY_SERVICE_ACCOUNT --repo DiegoTavares/thock --body "$DEPLOYER"
```

The release index is the same shape: a `thock-releases-deployer` account with `run.developer` on
`thock-releases-api`, and the variable `GCP_RELEASES_API_DEPLOY_SERVICE_ACCOUNT`. Its smoke test asks
for the stable macOS asset, the request every installed app makes.

## iPhone app

Manual for now; there is no upload pipeline and no App Store Connect key in the repository.

1. `thock/ios/script/test` and `thock/ios/script/smoke` pass.
2. Bump `CURRENT_PROJECT_VERSION` (every upload needs a new build number) and, for a new App Store
   version, `MARKETING_VERSION` in `Thock.xcodeproj`. All three targets share them. Commit as
   `thock: Version the phone app X.Y (build N)`.
3. In Xcode: scheme *Thock*, destination *Any iOS Device*, **Product → Archive**, then
   **Distribute App → App Store Connect**. Signing is automatic under team `KY338LSPPB`.
4. The build appears in TestFlight after processing. `ITSAppUsesNonExemptEncryption` is `NO`, so
   there is no export-compliance question.
5. Verify on a phone from TestFlight: open the practice notebook, then pair with a real desk.

**Undo:** expire the build in App Store Connect → TestFlight. A build that reached the App Store
can only be superseded.

A phone build talks to whatever Plus backend is live, so ship backend changes first and keep the
sync contract (`specs/v34-vault-sync-api.md`) backward compatible for a build that is already out.

## Checking what is live

```sh
curl -s https://storage.googleapis.com/thock-releases/channels/stable.json | jq '{version, released_at}'
gcloud run services list --project thock-505921 --format 'table(metadata.name, status.latestReadyRevisionName, status.url)'
gh run list --workflow release.yml --limit 3
gh run list --workflow deploy-services.yml --limit 3
```
