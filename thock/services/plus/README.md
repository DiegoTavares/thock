# Thock Plus backend

The service behind the hosted Thock Agent (spec: `thock/specs/v25-thock-plus-hosted-agent.md`,
Stage 1) and vault sync between the desk and the phone (`thock/specs/v34-vault-sync.md`, wire
contract `thock/specs/v34-vault-sync-api.md`). It owns users, entitlements, and a usage ledger in
Postgres; plans are rows you edit, not code; billing is absent by design (Polar arrives in Stage 2
as a driver that grants into this store). For sync it stores encrypted snapshots it cannot read and
queues the phone's writes until the desk applies them.

- `main.go`: the HTTP API and the allowance loop.
- `store.go`: the Postgres store (settings, plans, invites, users, ledger) over `pgx`.
- `db.go`: the connection pool and the migration runner.
- `migrations/`: numbered SQL files, embedded in the binary and applied once each at startup.
- `plans.go`: the plan shape and its validation.
- `gateway.go`: the OpenRouter provisioning-key driver and the fake used without a key.
- `vault.go`: the sync routes, the device-or-user auth, path validation, the sweeper.
- `vault_store.go`: vaults, devices, files, writes and pairings; versions and seqs under a row lock.
- `blobs.go`: the blob store interface and the local store that serves signed URLs itself.
- `feed.go`: the server-sent-events hub, the push interface with its logging fake, push coalescing.

No Dockerfile: Cloud Run buildpacks build it from source.

## Storage

`DATABASE_URL` is a `postgres://` URL. Startup connects, takes an advisory lock, applies every
migration in `migrations/` that `schema_migrations` doesn't list yet (each in its own
transaction), and only then serves traffic. `go run . migrate` does the same and exits, for
running a migration ahead of a deploy. Add a migration by adding `migrations/000N_<what>.sql`;
never edit an applied file.

The pool runs pgx in exec mode, which needs no server-side prepared statements, so a
transaction-mode pooler (Supabase's port 6543) works as the URL. The gateway key secrets live
in the `users` table: extractable by design (spec decision 12), bounded by their spend caps.

## How the allowance works

Each user gets one OpenRouter provisioned key capped at the plan's allowance in dollars, so the
gateway itself is the hard stop even if this service is down. On every entitlement read the
service pulls the key's cumulative spend (at most every 30 seconds), converts it to normalized
units (`units_per_dollar` in `settings`, 100 by default, so a unit is a cent), and:

- disables the key when the balance reaches zero (and re-enables it after a top-up),
- raises the key's cap when the allowance grows (top-up, plan change, new cycle),
- starts a fresh cycle from the current spend once `cycle_days` have passed.

The app polls `GET /v1/entitlement` after every agent turn; that is what the panel footer shows.
The sync-and-enforce step is serialized in the process, so run one instance.

## API

Errors are `{"error": "<a sentence the app shows as is>", "code": "<what the app branches on>"}`.
The sync routes' codes are listed in the contract (§3.3); the older routes carry a code derived from
the status (`unauthorized`, `revoked`, `not_found`, …).

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/connect` | none | `{"invite_code", "device"}` → `{"credential", "entitlement"}` |
| GET | `/v1/entitlement` | `Bearer <credential>` | plan, balance, model tiers, gateway key |
| POST | `/v1/disconnect` | `Bearer <credential>` | revoke the key and the credential |
| GET | `/admin/plans` | `Bearer <ADMIN_TOKEN>` | `units_per_dollar` and every plan |
| PUT | `/admin/plans/{id}` | admin | create or replace a plan (body: the plan JSON) |
| PUT | `/admin/settings` | admin | `{"units_per_dollar": N}` |
| POST | `/admin/invites` | admin | `{"plan", "max_uses", "note"}` → invite code |
| GET | `/admin/invites` | admin | list invites |
| GET | `/admin/users` | admin | list users (no secrets) |
| POST | `/admin/users/{id}/allowance` | admin | `{"reset": true}`, `{"adjust_units": N}`, `{"plan": "id"}` |
| POST | `/admin/users/{id}/revoke` | admin | kill the key, lock the credential (also lapses the user's vault) |
| GET | `/admin/users/{id}/ledger` | admin | the user's ledger entries |
| POST | `/admin/users/{id}/vault/lapse` | admin | `{"lapsed": true|false}`: what a billing driver does on cancel and renew |
| GET | `/health` | none | `ok` when the database answers |

The vault sync routes under `/v1/vault` are specified field by field in
`thock/specs/v34-vault-sync-api.md` §6 and are not repeated here. The desk calls them with its
Plus credential; the phone with the `tpp_…` credential pairing mints. `GET /v1/entitlement` carries
a `vault` object once the user has one, and a plan's `limits.vault_quota_bytes` (0 = no vault) is
what sizes it; the seeded `plus` and `dev` plans get 200 MB.

A plan body looks like this; `name`, `cycle_days` (30), `models.fast` (falls back to
`default`) and `limits.warn_at_percent` (80) are optional:

```json
{
  "name": "Thock Plus",
  "allowance_units": 1000,
  "cycle_days": 30,
  "models": {"default": "google/gemini-2.5-flash", "fast": "google/gemini-2.5-flash-lite"},
  "limits": {"warn_at_percent": 80, "max_turns_per_session": 200}
}
```

The first migration seeds `plus` and `dev` with those numbers. A revoked or unknown credential
answers 403/401; the app then falls back to the free BYO path.

## Tests

```sh
go test ./...
```

The tests run against a real Postgres. By default they start an embedded one (the binary is
downloaded once into the system temp dir, so the first run needs the network); set
`THOCK_PLUS_TEST_DATABASE_URL` to a server whose role may `create database` to use that
instead. Every test gets its own freshly migrated database.

## Local run

```sh
DATABASE_URL=postgres://... ADMIN_TOKEN=dev PORT=8080 go run .
# mint an invite for the seeded dev plan
curl -s -H 'Authorization: Bearer dev' -d '{"plan":"dev","max_uses":5,"note":"me"}' localhost:8080/admin/invites
```

Point the app at it with `THOCK_PLUS_URL=http://localhost:8080` (or `[plus] url` in the user-level
`settings.toml`). Without `OPENROUTER_MANAGEMENT_KEY` the keys are fake and no request reaches a
model, which is enough to exercise connect, balance, exhaustion (via `adjust_units`), and revocation.

## Running the sync server locally

With no bucket configured the service *is* the blob store: `BLOB_STORE=local` (the default) keeps
blobs as files under `BLOB_DIR` and hands out signed URLs that point back at
`PUBLIC_URL/v1/vault/blobs/{token}`. That makes `go run .` a complete sync server for one machine,
which is how the desk and the phone are developed against it.

```sh
DATABASE_URL=postgres://... ADMIN_TOKEN=dev PORT=8080 \
  BLOB_DIR=/tmp/thock-blobs PUBLIC_URL=http://192.168.1.20:8080 go run .
```

| Variable | Meaning |
|---|---|
| `BLOB_STORE` | `local` (default). A bucket-backed store implements `blobStore` in `blobs.go` and gets its own value. |
| `BLOB_DIR` | Where the local store keeps blobs; defaults to `thock-plus-blobs` under the system temp dir. |
| `BLOB_SIGNING_KEY` | HMAC key for the local store's URLs. Unset, a random per-process key is used, so URLs die with the process (fine for one machine; set it to survive restarts). |
| `PUBLIC_URL` | The base URL clients reach this process at, used in signed URLs. Defaults to `http://localhost:PORT`; set it to the LAN address when a phone on the same network should download. |

Pairing a test client by hand, with a desk credential from `POST /v1/connect`:

```sh
DESK=tpk_...
# the desk creates the vault (key_check is the first 32 hex of sha256("thock-vault-key-check/1" || key))
curl -s -H "Authorization: Bearer $DESK" -d '{"device_name":"my laptop","key_check":"0123456789abcdef0123456789abcdef"}' localhost:8080/v1/vault
# mint a pairing code and redeem it as the phone
curl -s -X POST -H "Authorization: Bearer $DESK" localhost:8080/v1/vault/pairings
curl -s -d '{"code":"K7MP-4QZX","device_name":"my phone","platform":"ios"}' localhost:8080/v1/vault/pair
# → {"credential":"tpp_...","device":{...},"vault":{...}}
PHONE=tpp_...
curl -s -H "Authorization: Bearer $PHONE" localhost:8080/v1/vault/files
curl -N -H "Authorization: Bearer $PHONE" localhost:8080/v1/vault/feed
```

Uploads are begin (`POST /v1/vault/files/{path}`), `PUT` the envelope to the returned URL, then
`POST /v1/vault/files/{path}/commit`. Without APNs credentials pushes are logged, not sent; the
`pusher` interface in `feed.go` is where a real client goes. A sweeper runs hourly and prunes lapsed
vaults after 30 days, uploads begun but never committed after an hour, tombstones after 30 days,
acked writes after a week, and spent pairing codes.

## Deploy

`deploy.sh` does the whole thing against the `thock-505921` project: it enables the APIs,
stores `DATABASE_URL` and `ADMIN_TOKEN` (and `OPENROUTER_MANAGEMENT_KEY` when given) in Secret
Manager, gives the service's own account access to them, and deploys from source with
`--max-instances 1`. Migrations run when the new revision starts.

```sh
DATABASE_URL='postgresql://...' ./deploy.sh              # first deploy, minting an admin token
OPENROUTER_MANAGEMENT_KEY='sk-or-...' ./deploy.sh        # add or rotate the gateway key
./deploy.sh                                              # redeploy the code only
```

Read the admin token back with `gcloud secrets versions access latest --secret thock-plus-admin`.
