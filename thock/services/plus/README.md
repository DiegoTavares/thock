# Thock Plus backend

The service behind the hosted Thock Agent (spec: `thock/specs/v25-thock-plus-hosted-agent.md`,
Stage 1). It owns users, entitlements, and a usage ledger; plans are configuration; billing is
absent by design (Polar arrives in Stage 2 as a driver that grants into this store).

- `main.go`: the HTTP API and the allowance loop.
- `plans.go`: the plan catalog, re-read from `plans.json` whenever the file changes.
- `store.go`: the JSON-file store (users, invites, ledger), rewritten atomically.
- `gateway.go`: the OpenRouter provisioning-key driver and the fake used without a key.
- `plans.example.json`: a starting plans file; copy it to `plans.json` and edit freely.

Go standard library only, no Dockerfile: Cloud Run buildpacks (or Fly/Railway) build it from source.

## How the allowance works

Each user gets one OpenRouter provisioned key capped at the plan's allowance in dollars, so the
gateway itself is the hard stop even if this service is down. On every entitlement read the
service pulls the key's cumulative spend (at most every 30 seconds), converts it to normalized
units (`units_per_dollar`, 100 by default, so a unit is a cent), and:

- disables the key when the balance reaches zero (and re-enables it after a top-up),
- raises the key's cap when the allowance grows (top-up, plan change, new cycle),
- starts a fresh cycle from the current spend once `cycle_days` have passed.

The app polls `GET /v1/entitlement` after every agent turn; that is what the panel footer shows.

## API

Errors are `{"error": "<a sentence the app shows as is>"}`.

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/connect` | none | `{"invite_code", "device"}` → `{"credential", "entitlement"}` |
| GET | `/v1/entitlement` | `Bearer <credential>` | plan, balance, model tiers, gateway key |
| POST | `/v1/disconnect` | `Bearer <credential>` | revoke the key and the credential |
| GET | `/admin/plans` | `Bearer <ADMIN_TOKEN>` | the live plans |
| POST | `/admin/plans/reload` | admin | force a re-read (edits are picked up anyway) |
| POST | `/admin/invites` | admin | `{"plan", "max_uses", "note"}` → invite code |
| GET | `/admin/invites` | admin | list invites |
| GET | `/admin/users` | admin | list users (no secrets) |
| POST | `/admin/users/{id}/allowance` | admin | `{"reset": true}`, `{"adjust_units": N}`, `{"plan": "id"}` |
| POST | `/admin/users/{id}/revoke` | admin | kill the key, lock the credential |
| GET | `/admin/users/{id}/ledger` | admin | the user's ledger entries |

A revoked or unknown credential answers 403/401; the app then falls back to the free BYO path.

## Tests

```sh
go test ./...
```

## Local run

```sh
cp plans.example.json plans.json
ADMIN_TOKEN=dev PORT=8080 go run .
# mint an invite for the dev plan
curl -s -H 'Authorization: Bearer dev' -d '{"plan":"dev","max_uses":5,"note":"me"}' localhost:8080/admin/invites
```

Point the app at it with `THOCK_PLUS_URL=http://localhost:8080` (or `[plus] url` in the user-level
`settings.toml`). Without `OPENROUTER_MANAGEMENT_KEY` the keys are fake and no request reaches a
model, which is enough to exercise connect, balance, exhaustion (via `adjust_units`), and revocation.

## Deploy

```sh
gcloud run deploy thock-plus-api \
  --source thock/services/plus \
  --region us-central1 \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 1 \
  --set-env-vars PLANS_PATH=/data/plans.json,STATE_PATH=/data/state.json \
  --set-secrets ADMIN_TOKEN=thock-plus-admin:latest,OPENROUTER_MANAGEMENT_KEY=openrouter-management:latest
```

The store is one JSON file, so mount a persistent volume at `/data` and keep `max-instances` at 1.
That is deliberate for Stage 1's handful of testers; the store interface is small enough to move
to a database when Polar and real users arrive. The gateway key secrets live in that file:
extractable by design (spec decision 12), bounded by their spend caps.
