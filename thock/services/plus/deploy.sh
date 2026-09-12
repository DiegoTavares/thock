#!/usr/bin/env bash
# Deploys the Thock Plus backend to Cloud Run from source.
#
# Secrets come from the environment and go into Secret Manager; a variable
# that is unset keeps the secret's current version. The first run needs
# DATABASE_URL and mints ADMIN_TOKEN unless one is given.
set -euo pipefail

PROJECT="${PROJECT:-thock-505921}"
REGION="${REGION:-us-central1}"
SERVICE="${SERVICE:-thock-plus-api}"
ACCOUNT_NAME="${SERVICE}"
ACCOUNT="${ACCOUNT_NAME}@${PROJECT}.iam.gserviceaccount.com"
SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '%s\n' "$*" >&2; }

gcloud services enable run.googleapis.com cloudbuild.googleapis.com secretmanager.googleapis.com \
  artifactregistry.googleapis.com --project "$PROJECT" --quiet

if ! gcloud iam service-accounts describe "$ACCOUNT" --project "$PROJECT" >/dev/null 2>&1; then
  log "creating the service account $ACCOUNT"
  gcloud iam service-accounts create "$ACCOUNT_NAME" --project "$PROJECT" \
    --display-name "Thock Plus API" --quiet
fi

# put_secret <name> <value>: creates the secret on first use, then adds a
# version, and lets the service account read it.
put_secret() {
  local name="$1" value="$2"
  if ! gcloud secrets describe "$name" --project "$PROJECT" >/dev/null 2>&1; then
    gcloud secrets create "$name" --project "$PROJECT" --replication-policy automatic --quiet
  fi
  printf '%s' "$value" | gcloud secrets versions add "$name" --project "$PROJECT" --data-file=- --quiet
  gcloud secrets add-iam-policy-binding "$name" --project "$PROJECT" \
    --member "serviceAccount:$ACCOUNT" --role roles/secretmanager.secretAccessor --quiet >/dev/null
}

has_secret() {
  gcloud secrets versions list "$1" --project "$PROJECT" --filter 'state=enabled' --format 'value(name)' 2>/dev/null | grep -q .
}

if [[ -n "${DATABASE_URL:-}" ]]; then
  put_secret thock-plus-database-url "$DATABASE_URL"
elif ! has_secret thock-plus-database-url; then
  log "DATABASE_URL is required on the first deploy (a postgres:// URL)."
  exit 1
fi

if [[ -n "${ADMIN_TOKEN:-}" ]]; then
  put_secret thock-plus-admin "$ADMIN_TOKEN"
elif ! has_secret thock-plus-admin; then
  log "minting an admin token; read it back with: gcloud secrets versions access latest --secret thock-plus-admin"
  put_secret thock-plus-admin "$(openssl rand -hex 24)"
fi

SECRETS="DATABASE_URL=thock-plus-database-url:latest,ADMIN_TOKEN=thock-plus-admin:latest"
if [[ -n "${OPENROUTER_MANAGEMENT_KEY:-}" ]]; then
  put_secret openrouter-management "$OPENROUTER_MANAGEMENT_KEY"
fi
if has_secret openrouter-management; then
  SECRETS="$SECRETS,OPENROUTER_MANAGEMENT_KEY=openrouter-management:latest"
else
  log "no openrouter-management secret: the service will mint fake gateway keys"
fi

gcloud run deploy "$SERVICE" \
  --project "$PROJECT" \
  --region "$REGION" \
  --source "$SOURCE" \
  --service-account "$ACCOUNT" \
  --allow-unauthenticated \
  --min-instances 0 --max-instances 1 \
  --cpu 1 --memory 256Mi \
  --set-secrets "$SECRETS" \
  --quiet

gcloud run services describe "$SERVICE" --project "$PROJECT" --region "$REGION" --format 'value(status.url)'
