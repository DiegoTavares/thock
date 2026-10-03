# Thock V36 — A circuit breaker on the cloud bill

**Status:** In progress (built on `budget-guard`; not yet deployed)
**Owner:** Diego · **Date:** 2026-10-03
**Companion docs:** `v20-auto-update.md` (the public releases bucket), `v21-site-hosting-and-download-gate.md`
(why the artifacts are public), `../services/plus/README.md`

---

## 1. Summary

Every Thock service is public: three Cloud Run services take unauthenticated traffic and
`gs://thock-releases` serves ~125 MB installers to anyone. GCP budgets only *notify*; nothing stops
spending. Max-instance caps already bound compute (about USD 1.3k/month across all services at full
saturation), but egress is unbounded. A script looping over one installer at 1 Gbps costs roughly
USD 1,300 a day in internet egress from the bucket alone.

V36 adds **budget-guard**, a small Go service that listens to budget notifications and, once actual
spend for a project crosses a trip ratio, takes that project's public surface offline: Cloud Run
services go to internal-only ingress and public buckets lose their `allUsers` binding. Restoring is
one script. A bill that would have been four figures stops at a known multiple of the budget, plus
the billing data lag.

The trade is explicit: when the guard trips, Thock Plus, the site and the updater are down until
Diego restores them. That is the right failure for a product at beta scale.

## 2. What already exists (2026-10-03)

- Account budget, CAD 30/month: email at 50/90/100/150% actual and 100% forecast.
- `thock-505921` budget, CAD 20/month: email at 50/90/100/200% actual and 100% forecast.
- Both also notify the `diego@studiobeehive.ca` Monitoring email channel.
- Billing export to BigQuery `thock-505921.billing_export`.
- Max instances: `thock-plus-api` 1, `thock-releases-api` 3, `thock-site` 3.

## 3. Decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Trigger | **Budget Pub/Sub notifications, actual spend only.** The budget publishes to topic `budget-alerts` several times a day with `costAmount` and `budgetAmount`; the guard computes the ratio itself rather than trusting `alertThresholdExceeded`. Forecast spend is not used: it is noisy early in the month and would trip on a single busy day. |
| 2 | Trip ratio | **2.0× the budget** (CAD 40 for `thock-505921`), configurable per budget. Email alerts at 0.5–1.5× give Diego time to act by hand first; the guard is the backstop, not the first line. |
| 3 | Scope | **One budget per protected project**; the guard acts only on the project its config maps that budget to (`BUDGETS=<budget id>=<project>[@<ratio>]`); notifications for unmapped budgets, including the account-wide one, are ignored, so a spike in one project never takes another offline. The map is deploy-time config, not committed, so other personal projects can be protected without naming them in this public repo. |
| 4 | Cloud Run action | **Set ingress to `internal`**, patching only `ingress` and `labels` through an update mask so no new revision rolls out. Public requests are rejected at Google's front end, no instance starts, and the service, its revisions and its IAM stay intact. A service already internal is left alone and unlabelled, so a restore never opens it up. Removing the `allUsers` invoker binding was the alternative; it still lets unauthenticated traffic reach the front end, and restoring it is easier to get wrong. Max instances cannot be set to 0. |
| 5 | Bucket action | **Remove `allUsers` and `allAuthenticatedUsers` from every bucket binding** in the project, so objects stop being publicly readable. Signed URLs (Plus blobs) keep working, since they don't depend on either. A public binding on a custom role is still removed but can't be recorded in a label; it is logged for re-adding by hand. |
| 6 | What is never touched | Anything labelled `budget-guard=exempt` (the guard's own service, set by `deploy.sh`), Firestore, Secret Manager, the billing link. **Unlinking billing is rejected**: it stops every service and starts deletion clocks on stored data, which is worse than the bill it prevents at this scale. |
| 7 | Idempotency | Pub/Sub delivers at least once and budgets republish all day. Each action is a no-op when already applied. What a trip changed is recorded on each touched resource: `budget-guard=tripped`, the previous ingress, and one `budget-guard-public-N` label per removed bucket binding. Bucket labels are written before the policy, so a failure in between still leaves something to restore. |
| 8 | Restore | **Manual.** `budget-guard restore [-dry-run] <project>` (a subcommand of the same binary, run with Diego's credentials) puts back the recorded ingress and bindings on resources labelled tripped, clears those labels, and sets `budget-guard-restored=<YYYY-MM>`. The budget keeps reporting the overspend for the rest of the month, so the guard skips resources restored in the current period; restoring therefore disarms it for that project until the next month. No auto-restore: if the cause was abuse, it is still there. |
| 9 | Telling Diego | One structured log line per action (`jsonPayload.event="budget_guard"`, dry run included), and a log-based alert policy, "Thock budget guard acted", created by `deploy.sh` on the given email channel. |
| 10 | Dry run | `DRY_RUN=true` logs the actions it would take without making them. `deploy.sh` defaults to it, so the first deploy runs dry for one billing cycle. |

## 4. Topology

```
Budget (per project) ──► Pub/Sub topic budget-alerts ──push (OIDC)──► Cloud Run budget-guard
                                                                        ├─ run.services.update  ingress=internal
                                                                        └─ storage.buckets.setIamPolicy  −allUsers
```

- Lives in `thock/services/budget-guard/`, deployed with a `deploy.sh` like `plus/`, in
  `thock-505921`, `us-central1`, max instances 1.
- Runtime service account `budget-guard@thock-505921`, holding a custom role `budgetGuard` in each
  protected project: `run.services.{list,get,update}`, `storage.buckets.{list,get,update,getIamPolicy,setIamPolicy}`.
  Updating a service requires `iam.serviceAccounts.actAs` on its runtime account; `deploy.sh` grants
  `serviceAccountUser` per account, not project-wide, so re-run it after adding a service.
- The guard's own service: no `allUsers` invoker; the push subscription authenticates with an OIDC
  token for a dedicated `budget-guard-pusher@` account holding `run.invoker` on it alone.
- Budgets connect with `gcloud billing budgets update … --notifications-rule-pubsub-topic`.

## 5. Message handling

The notification body is JSON with `costAmount`, `budgetAmount`, `currencyCode`, `costIntervalStart`;
attributes carry `budgetId` and `billingAccountId`. The guard:

1. Acks and ignores messages for budgets not in its config, or with `budgetAmount` ≤ 0.
2. Trips when `costAmount / budgetAmount ≥ ratio` for the current `costIntervalStart`.
3. Lists Cloud Run services (all regions) and buckets in the configured project, skips exempt and
   restored-this-period resources, applies §3.4 and §3.5, labels, logs. Trips are serialized and run
   detached from the push request, bounded at two minutes.
4. Returns 2xx even when an action fails after logging it, so Pub/Sub does not hot-retry; the
   next notification (≈20–60 minutes later) retries naturally. A malformed body is logged and acked.

## 6. Limits

- **Billing lag.** Cost data trails usage by hours (up to a day). At 1 Gbps of abuse the guard trips
  at the ratio plus whatever accrued during the lag — still bounded, still far better than a month.
- **Egress already in flight** finishes; downloads that started before the bucket went private run
  to completion.
- Does not cover spend outside GCP (OpenRouter is already capped per user by the Plus gateway keys).

## 7. Non-goals

- Moving release downloads off the public bucket (R2, GitHub Releases, Cloud CDN). Deferred by
  choice on 2026-10-03; the guard bounds the risk meanwhile.
- Per-IP rate limiting. Not available for backend buckets, and the guard is cheaper.
- Auto-restore.

## 8. Tests

- `go test ./...`: the budget notification fixture; ratio boundaries; unknown budgets, zero budgets and
  malformed pushes acked without action; exempt, already-internal and restored-this-period resources
  skipped; second trip makes no writes; a failing action doesn't stop the rest; dry run makes no
  calls; trip→restore round trip, including a binding the trip dropped entirely and a restore that
  sticks against the next notification; every predefined Storage role round-trips through a label.
  The REST client is tested against a stand-in server for paths, the update mask, pagination, error
  bodies, `[]` bindings, and label removal as `null`.
- `deploy.sh` passes shellcheck. The update-mask PATCH was checked against the live Cloud Run API with
  `validateOnly=true`.
- `thock/script/test` maps `thock/services/budget-guard/*` to a `go-budget-guard` suite.
- Live, in dry run: `gcloud pubsub topics publish budget-alerts` with a synthetic 2.1× message and
  check the log line; then one real trip against a throwaway project before enabling it on
  `thock-505921`.
