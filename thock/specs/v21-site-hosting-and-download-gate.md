# Thock V21 — The website, and a door with a code on it

**Status:** Shipped (2026-09-04)
**Owner:** Diego · **Date:** 2026-09-04
**Companion docs:** `v20-auto-update.md` (the release index and manifest this page reads; §16 left
"where does the download page live" open — this answers it), `v20-auto-update-rollout.md`,
`../site/README.md`

---

## 1. Summary

`thock/site/index.html` existed and was hosted nowhere. V20 gave installed apps a place to fetch
updates from; nobody yet had a place to fetch the *first* install from. V21 puts the site on
`https://thethock.com`, inside the same GCP project as the release index, and adds a `/download`
page that lists the current build — but only to someone holding an **invite code** that arrives by
email after waitlist approval.

The constraint that shaped it: **as little backend as possible.** The gate is done with a sealed
blob and the browser's own crypto, and the page reads the release manifest CI already writes, so
a new release changes the download page without anyone deploying it. The one server-side route,
`POST /waitlist`, exists because the alternative was paying a form service (§2 decision 3).

## 2. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Hosting | **Cloud Run, from source**, a 60-line Go static server that embeds `public/`. Same project, region, toolchain, and domain-mapping mechanism as `updates.thethock.com`, and the one option that needed no new CLI or login. Firebase Hosting was the alternative (free CDN, no cold start) at the cost of a second toolchain and a console-driven custom domain; it is the swap if the ~1 s cold start on a first visit ever matters. `--min-instances 1` is the cheaper fix if it does. |
| 2 | Gate mechanism | **A sealed blob, decrypted in the page.** `gate.json` = the manifest URL encrypted with AES-256-GCM under a key derived from the invite code (PBKDF2-SHA256, 600k rounds, random salt). A wrong code fails GCM authentication; there is no plaintext hash to compare against and nothing server-side to bypass. The obvious cheaper option — an unguessable URL path — protects the same thing with worse ergonomics (a leaked link can't be rotated without breaking every email already sent). |
| 3 | What "approval" is | **A human sends an email.** Signups POST to the site's own `/waitlist`, which writes one Firestore document per address and logs a structured line; a Cloud Logging alert emails Diego the address. Approving is replying with the code, or a `/download#THOCK-…` link that pre-fills it. No accounts, no tokens per user, no third-party form service. *(Revised 2026-09-04: the first cut named Formspree; Diego chose to drop the no-backend constraint rather than pay for a form service. The ~120 lines of Go this added are the only server-side logic on the site.)* |
| 4 | One code, shared | A single live code for the beta cohort, rotated by re-sealing and redeploying. Per-tester codes would need a list somewhere, which is a backend. Revisit when the cohort is large enough that one leaked code matters. |
| 5 | Where the build info comes from | **The page fetches `channels/stable.json` from the releases bucket** after unlocking. Version, per-platform URLs, and checksums are the manifest's fields; the page renders them. A release is therefore a tag, exactly as V20 says — the site never has to know a version. Requires a read-only CORS rule on the bucket (`origin: *`, `GET`); the bucket is public anyway. |
| 6 | What the gate does *not* protect | The artifacts. They are public objects because the auto-updater downloads them with no credentials (V20 §6). The gate keeps the links off the open web and out of search indexes (`noindex` on the page); anyone with a URL can fetch. That is the right amount of protection for a private beta and is stated in the README rather than pretended otherwise. |
| 7 | Invite code shape | `THOCK-XXXX-XXXX-XXXX-XXXX`, base32 without look-alike glyphs, 80 bits. `gate.json` is public, so the code must survive an offline guess; 80 bits at 600k PBKDF2 rounds does. Dashes, spaces, and case are ignored on entry. |
| 8 | www | Mapped to the same service; the server 301s to the apex. |

## 3. Topology

```
thethock.com, www.thethock.com  ──►  Cloud Run  thock-site  (us-central1)
                                       └─ embeds public/{index.html, download.html, gate.json}
/download  ─(code)─►  decrypt gate.json  ─►  GET https://storage.googleapis.com/thock-releases/channels/stable.json
                                              └─ render version, Download buttons (dist/vX/…), sha256
```

- Service account `thock-site@thock-505921.iam.gserviceaccount.com`, **no roles** — a static
  server needs nothing, and the default compute account carries Editor.
- Domain mappings `thethock.com` (A/AAAA) and `www.thethock.com` (CNAME) in the `thethock-com`
  Cloud DNS zone, beside the existing `updates` record.
- `gs://thock-releases` gained one CORS rule. Nothing else about V20's infrastructure changed.
- Waitlist: Firestore `(default)` (Native, `us-central1`), collection `waitlist`; `thock-site@` holds
  `roles/datastore.user`; Monitoring notification channel (email) + log-based alert policy "Thock waitlist
  signup" matching `jsonPayload.event="waitlist_signup"` on the service, 5-minute notification rate limit.
- Service account `thock-site-deployer@…` for CI: `cloudbuild.builds.editor` (project), `run.developer`
  on `thock-site` only, `storage.admin` on the Cloud Run sources bucket, `artifactregistry.writer` on
  `cloud-run-source-deploy`, `serviceAccountUser` on the runtime and default build accounts, and
  `workloadIdentityUser` from the `github` pool — the same pool and repository condition as V20.

## 4. Operating it

| Task | How |
|---|---|
| Deploy | Merge to `main`. `.github/workflows/deploy-site.yml` tests, deploys and smoke-tests on any push touching `thock/site/**`, as `thock-site-deployer@` via the V20 WIF pool. By hand: `gcloud run deploy thock-site --source thock/site …` (README). |
| Approve a tester | Reply to their signup with `https://thethock.com/download#<code>` |
| Rotate the code | `node thock/site/seal.mjs`, redeploy, re-send |
| New release | Nothing — push the tag (V20). The page reads the manifest. |
| See the waitlist | Firestore console, or the `curl` in the README |
| Change who gets signup emails | Edit the Monitoring notification channel, or add one to the policy |

## 5. Non-goals

- Per-tester codes, revocation, download counts — the natural next step now that Firestore is there
  (per-tester invite documents checked by an `/unlock` route), deferred until one shared code stings.
- Protecting the artifact bytes. See decision 6; changing that means signed URLs and a change to
  the updater's contract.
- A CDN in front of the site. Cloud Run serves a 40 KB page fine at beta scale.

## 6. Tests

- `go test ./...` in `thock/site`: index served, `/download` clean URL, `gate.json` served and
  contains no plaintext bucket URL, unknown path 404s, `/health`, `www` redirect. `/waitlist`
  against a fake Firestore: document id and fields, bearer token, structured log line, repeat signup
  is a quiet 200, bad input is a 400 that never reaches Firestore, Firestore down is a 503, GET is 405.
- The seal/unseal pair was round-tripped in Node with the page's exact algorithm: the right code
  (lowercase, dashes stripped) decrypts; a one-character change is rejected by GCM.
- Live: `curl https://thethock.com/health`, open `/download`, enter the code, see the build.
