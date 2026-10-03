# Budget guard

A circuit breaker on the GCP bill. It receives Cloud Billing budget notifications through Pub/Sub
and, once a project's actual spend reaches a set multiple of its budget (2× by default), switches
the project's Cloud Run services to internal-only ingress and removes `allUsers` /
`allAuthenticatedUsers` from its buckets. Spec: `thock/specs/v36-budget-guard.md`.

- `main.go` — the push endpoint, configuration, and the `restore` subcommand.
- `guard.go` — trip and restore, over a small `cloud` interface.
- `cloud.go` — that interface over the Cloud Run Admin v2 and Cloud Storage JSON REST APIs.
- `deploy.sh` — service, accounts, custom role, topic, push subscription, budget wiring, alert.

## How it remembers

It keeps no state. Each resource it touches gets labels:

| Label | Meaning |
|---|---|
| `budget-guard=tripped` | Restricted by the guard; `restore` acts only on these. |
| `budget-guard-ingress` | The service's ingress before the trip. |
| `budget-guard-public-N` | One public bucket binding before the trip, e.g. `allusers_objectviewer`. |
| `budget-guard-restored=2026-10` | Restored by hand this billing month; the guard leaves it alone until the next. |
| `budget-guard=exempt` | Set by hand; never touched. `deploy.sh` sets it on the guard's own service. |

## Configuration

| Variable | |
|---|---|
| `BUDGETS` | Comma-separated `<budget id>=<project>[@<ratio>]`. Notifications for other budgets are ignored. |
| `DRY_RUN` | `true` logs every action it would take and changes nothing. |

## Tests

```sh
go test ./...
```

## Deploy

```sh
BILLING_ACCOUNT=01C412-8C5F16-6E3B0F \
BUDGETS='<thock budget id>=thock-505921' \
ALERT_CHANNEL=projects/thock-505921/notificationChannels/<id> \
./deploy.sh
```

The first deploy is a dry run. After a billing cycle of logs (`jsonPayload.event="budget_guard"`)
that look right, deploy again with `DRY_RUN=false`. To exercise it without waiting for real spend,
publish a fake notification over the trip ratio:

```sh
gcloud pubsub topics publish budget-alerts --project thock-505921 \
  --attribute budgetId=<budget id>,schemaVersion=1.0 \
  --message '{"costAmount":41,"budgetAmount":20,"currencyCode":"CAD","costIntervalStart":"2026-10-01T07:00:00Z"}'
```

## After a trip

Find the cause first (the billing export in `thock-505921.billing_export`). Then:

```sh
go run . restore -dry-run thock-505921   # what would change
go run . restore thock-505921
```

Restore uses your own gcloud application-default credentials.
