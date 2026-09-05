# Thock website

`https://thethock.com` — the landing page, the waitlist, and the invite-gated download page.
Spec: `thock/specs/v21-site-hosting-and-download-gate.md`.

```
public/index.html      the landing page (no build step; Google Fonts is the only dependency)
public/download.html   /download — asks for an invite code, then lists the current build
public/gate.json       the release-manifest URL, sealed under the invite code (see seal.mjs)
main.go                a static file server: embeds public/, nothing else
seal.mjs               writes gate.json; prints the invite code
```

## Hosting

Cloud Run, from source, in the same project and region as the release index
(`thock/services/releases`). Buildpacks compile `main.go`; there is no Dockerfile.

**Deploys are automatic:** `.github/workflows/deploy-site.yml` runs on every push to `main` that
touches this directory (tests, then deploy, then a smoke test), authenticating through the same
Workload Identity Federation pool as the release workflows as `thock-site-deployer@`, an account
that can build and roll out this one service and nothing else. The job skips until the
`GCP_SITE_DEPLOY_SERVICE_ACCOUNT` repository variable is set. To deploy by hand:

```sh
gcloud run deploy thock-site \
  --source thock/site \
  --region us-central1 \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 3 \
  --service-account thock-site@thock-505921.iam.gserviceaccount.com
```

`thethock.com` and `www.thethock.com` are Cloud Run domain mappings onto the service (the server
redirects `www` to the apex); the records live in the `thethock-com` Cloud DNS zone. Redeploying
never touches them.

Tests: `go test ./...` from this directory.

## Download gate

The page at `/download` is the only place the download links appear, and it shows them only after
the visitor enters an **invite code**. The code is not checked by a server: `gate.json` holds the
URL of the release manifest encrypted under the code (PBKDF2-SHA256, 600k rounds → AES-256-GCM),
and the browser decrypts it. A wrong code fails authentication and reveals nothing. Once open, the
page fetches `channels/stable.json` straight from the releases bucket and renders the version,
one download button per platform, and the checksum — so a new release needs no site deploy.

Approval is therefore an email: someone joins the waitlist, you decide, you send them the code
(`https://thethock.com/download#THOCK-…` opens the page pre-filled; the fragment never reaches
the server).

Rotate the code whenever you like:

```sh
node seal.mjs             # new code, printed once
node seal.mjs THOCK-…     # re-seal under a code you already sent
gcloud run deploy …       # as above
```

What the gate is and isn't: it keeps the links off the open web and out of search, which is all a
private beta needs. The artifacts themselves are public objects (the auto-updater fetches them
with no credentials), so anyone who already has a URL can download; the gate guards discovery,
not the bytes. The releases bucket has a read-only `*` CORS rule so the page can fetch the
manifest; that rule was added for this page and nothing else depends on it.

## Waitlist

No third party. The two forms POST `{ "email": "..." }` to the site's own `/waitlist` route
(`WAITLIST_ENDPOINT` in `public/index.html`; set it to `""` to close signups — the form then says
so). The server normalizes the address, writes one document per address to the Firestore
collection `waitlist` (id = hash of the address, so a repeat signup is a no-op), and logs a
structured `waitlist_signup` line. A Cloud Logging alert policy ("Thock waitlist signup") matches
that line and emails the notification channel — currently `diego@studiobeehive.ca` — with the
address in the message. Signups within five minutes of each other collapse into one email.

Approving is replying with the invite link (see above). The full list:

```sh
curl -s -H "Authorization: Bearer $(gcloud auth print-access-token)" \
  "https://firestore.googleapis.com/v1/projects/thock-505921/databases/(default)/documents/waitlist?pageSize=300" \
  | python3 -c 'import json,sys; [print(d["fields"]["joined_at"]["timestampValue"][:10], d["fields"]["email"]["stringValue"]) for d in json.load(sys.stdin).get("documents",[])]'
```

Pieces, all in project `thock-505921`: Firestore `(default)`, Native mode, `us-central1`; the
runtime account `thock-site@` holds `roles/datastore.user`; notification channel and alert policy
in Cloud Monitoring. The server reaches Firestore over REST with the metadata-server token — no
SDK, no key.

## Design

Direction: "Low Light". Warm charcoal ground (`#1A1918`), amber accent (`#E2A554`),
Schibsted Grotesk for text, Geist Mono for keys/paths/demos. Deliberately dark-only for now.

The page is keyboard-navigable (`j`/`k` between sections, `gg`/`G`, `w` for waitlist, `?` for the
keymap sheet), matching the product's keyboard-first principle.

Copy rules: descriptive voice, no selling; vault compatibility stays implicit ("point Thock at the
vault you already keep"); the word "Obsidian" does not appear.
