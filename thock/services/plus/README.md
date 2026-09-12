# Thock Plus backend

The service behind the hosted Thock Agent (spec: `thock/specs/v25-thock-plus-hosted-agent.md`,
Stage 1). It owns users, entitlements, and a usage ledger in Postgres; plans are rows you edit,
not code; billing is absent by design (Polar arrives in Stage 2 as a driver that grants into this
store).

- `main.go`: the HTTP API and the allowance loop.
- `store.go`: the Postgres store (settings, plans, invites, users, ledger) over `pgx`.
- `db.go`: the connection pool and the migration runner.
- `migrations/`: numbered SQL files, embedded in the binary and applied once each at startup.
- `plans.go`: the plan shape and its validation.
- `gateway.go`: the OpenRouter provisioning-key driver and the fake used without a key.

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

Errors are `{"error": "<a sentence the app shows as is>"}`.

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
| POST | `/admin/users/{id}/revoke` | admin | kill the key, lock the credential |
| GET | `/admin/users/{id}/ledger` | admin | the user's ledger entries |
| GET | `/health` | none | `ok` when the database answers |

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
