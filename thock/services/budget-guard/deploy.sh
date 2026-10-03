#!/usr/bin/env bash
# Deploys the budget guard and wires it to the budgets it watches
# (spec: thock/specs/v36-budget-guard.md). Safe to re-run; re-run it after
# adding a Cloud Run service to a protected project, so the guard can act on
# the new service's runtime account.
#
#   BILLING_ACCOUNT=01C412-… BUDGETS='<budget id>=<project>[@<ratio>];…' ./deploy.sh
#
# BUDGETS is semicolon-separated here (commas inside gcloud flags are taken).
# DRY_RUN defaults to true: the guard logs what it would do and changes
# nothing. Deploy with DRY_RUN=false once a cycle of dry-run logs looks right.
# ALERT_CHANNEL, a Monitoring notification channel name, gets an email for
# every action the guard takes.
set -euo pipefail

PROJECT="${PROJECT:-thock-505921}"
REGION="${REGION:-us-central1}"
SERVICE="${SERVICE:-budget-guard}"
TOPIC="${TOPIC:-budget-alerts}"
DRY_RUN="${DRY_RUN:-true}"
ACCOUNT="${SERVICE}@${PROJECT}.iam.gserviceaccount.com"
PUSHER="${SERVICE}-pusher@${PROJECT}.iam.gserviceaccount.com"
ROLE_ID=budgetGuard
SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '%s\n' "$*" >&2; }

: "${BILLING_ACCOUNT:?set BILLING_ACCOUNT to the billing account id}"
: "${BUDGETS:?set BUDGETS to <budget id>=<project>[@<ratio>];…}"

budget_ids=()
protected=()
IFS=';' read -ra entries <<<"$BUDGETS"
for entry in "${entries[@]}"; do
  [[ -n "$entry" ]] || continue
  budget_ids+=("${entry%%=*}")
  target="${entry#*=}"
  protected+=("${target%%@*}")
done
mapfile -t protected < <(printf '%s\n' "${protected[@]}" | sort -u)

gcloud services enable run.googleapis.com cloudbuild.googleapis.com artifactregistry.googleapis.com \
  pubsub.googleapis.com billingbudgets.googleapis.com monitoring.googleapis.com \
  --project "$PROJECT" --quiet

ensure_account() {
  local email="$1" display="$2"
  if ! gcloud iam service-accounts describe "$email" --project "$PROJECT" >/dev/null 2>&1; then
    log "creating the service account $email"
    gcloud iam service-accounts create "${email%%@*}" --project "$PROJECT" --display-name "$display" --quiet
  fi
}
ensure_account "$ACCOUNT" "Budget guard"
ensure_account "$PUSHER" "Budget guard Pub/Sub push"

# Exactly what trip and restore call, in each project the guard protects.
PERMISSIONS=run.services.list,run.services.get,run.services.update,storage.buckets.list,storage.buckets.get,storage.buckets.update,storage.buckets.getIamPolicy,storage.buckets.setIamPolicy
for project in "${protected[@]}"; do
  log "granting the guard its role in $project"
  if gcloud iam roles describe "$ROLE_ID" --project "$project" >/dev/null 2>&1; then
    gcloud iam roles update "$ROLE_ID" --project "$project" --permissions "$PERMISSIONS" --quiet >/dev/null
  else
    gcloud iam roles create "$ROLE_ID" --project "$project" --title "Budget guard" \
      --description "Restricts public Cloud Run services and buckets when spend runs away (Thock spec v36)." \
      --permissions "$PERMISSIONS" --quiet >/dev/null
  fi
  gcloud projects add-iam-policy-binding "$project" --member "serviceAccount:$ACCOUNT" \
    --role "projects/$project/roles/$ROLE_ID" --condition None --quiet >/dev/null
  # Updating a Cloud Run service requires acting as its runtime account;
  # granted per account rather than project-wide.
  while read -r runtime; do
    [[ -n "$runtime" ]] || continue
    gcloud iam service-accounts add-iam-policy-binding "$runtime" --project "${runtime#*@}" \
      --member "serviceAccount:$ACCOUNT" --role roles/iam.serviceAccountUser --quiet >/dev/null 2>&1 ||
      log "could not grant actAs on $runtime; the guard may fail to restrict the services using it"
  done < <(gcloud run services list --project "$project" --format 'value(spec.template.spec.serviceAccountName)' | sort -u)
done

# Labelled exempt so the guard never restricts itself; no public invoker, the
# push subscription authenticates as $PUSHER.
gcloud run deploy "$SERVICE" \
  --project "$PROJECT" \
  --region "$REGION" \
  --source "$SOURCE" \
  --service-account "$ACCOUNT" \
  --no-allow-unauthenticated \
  --labels budget-guard=exempt \
  --min-instances 0 --max-instances 1 \
  --cpu 1 --memory 256Mi \
  --timeout 300 \
  --set-env-vars "^|^BUDGETS=${BUDGETS//;/,}|DRY_RUN=$DRY_RUN" \
  --quiet
URL="$(gcloud run services describe "$SERVICE" --project "$PROJECT" --region "$REGION" --format 'value(status.url)')"
gcloud run services add-iam-policy-binding "$SERVICE" --project "$PROJECT" --region "$REGION" \
  --member "serviceAccount:$PUSHER" --role roles/run.invoker --quiet >/dev/null

if ! gcloud pubsub topics describe "$TOPIC" --project "$PROJECT" >/dev/null 2>&1; then
  gcloud pubsub topics create "$TOPIC" --project "$PROJECT" --quiet
fi
# Pub/Sub mints the push subscription's OIDC tokens as $PUSHER.
PROJECT_NUMBER="$(gcloud projects describe "$PROJECT" --format 'value(projectNumber)')"
gcloud iam service-accounts add-iam-policy-binding "$PUSHER" --project "$PROJECT" \
  --member "serviceAccount:service-${PROJECT_NUMBER}@gcp-sa-pubsub.iam.gserviceaccount.com" \
  --role roles/iam.serviceAccountTokenCreator --quiet >/dev/null
subscription_flags=(--push-endpoint "$URL/pubsub" --push-auth-service-account "$PUSHER" --ack-deadline 180)
if gcloud pubsub subscriptions describe "$SERVICE" --project "$PROJECT" >/dev/null 2>&1; then
  gcloud pubsub subscriptions update "$SERVICE" --project "$PROJECT" "${subscription_flags[@]}" --quiet
else
  gcloud pubsub subscriptions create "$SERVICE" --project "$PROJECT" --topic "$TOPIC" "${subscription_flags[@]}" --quiet
fi

for budget in "${budget_ids[@]}"; do
  log "sending budget $budget to $TOPIC"
  gcloud billing budgets update "$budget" --billing-account "$BILLING_ACCOUNT" --billing-project "$PROJECT" \
    --notifications-rule-pubsub-topic "projects/$PROJECT/topics/$TOPIC" --quiet >/dev/null
done

if [[ -n "${ALERT_CHANNEL:-}" ]]; then
  api="https://monitoring.googleapis.com/v3/projects/$PROJECT/alertPolicies"
  token="$(gcloud auth print-access-token)"
  existing="$(curl -fsS -G -H "Authorization: Bearer $token" "$api" \
    --data-urlencode 'filter=display_name="Thock budget guard acted"' | grep -c '"name"' || true)"
  if [[ "$existing" == "0" ]]; then
    log "creating the alert policy"
    curl -fsS -H "Authorization: Bearer $token" -H 'Content-Type: application/json' "$api" -d @- >/dev/null <<EOF
{
  "displayName": "Thock budget guard acted",
  "combiner": "OR",
  "conditions": [{
    "displayName": "budget_guard log line",
    "conditionMatchedLog": {
      "filter": "resource.type=\"cloud_run_revision\" resource.labels.service_name=\"$SERVICE\" jsonPayload.event=\"budget_guard\""
    }
  }],
  "alertStrategy": {"notificationRateLimit": {"period": "300s"}},
  "notificationChannels": ["$ALERT_CHANNEL"],
  "documentation": {"content": "The budget guard restricted or restored resources (or would have, in a dry run). Spec: thock/specs/v36-budget-guard.md. Undo: go run ./thock/services/budget-guard restore <project>", "mimeType": "text/markdown"}
}
EOF
  fi
else
  log "ALERT_CHANNEL unset: no email when the guard acts"
fi

log "deployed $URL (dry run: $DRY_RUN), protecting: ${protected[*]}"
