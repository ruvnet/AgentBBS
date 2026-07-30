#!/usr/bin/env bash
# Deploy agentbbs-web to Cloud Run, wired to the live meta-llm cog_ gateway.
#
# Identity: the darwin-nightly service account (headless, no reauth) — has
# run.admin + artifactregistry.writer + secretmanager.admin, so it can build/push
# to AR, deploy Cloud Run, and read the secret at runtime.
#
# SECURITY: credentials are injected ONLY as Secret Manager references via
# --set-secrets (never a literal); nothing secret is printed or committed. The
# static GitHub Pages site never holds the key — it talks to THIS server, which
# holds the key server-side and proxies to meta-llm.
set -euo pipefail

PROJECT=cognitum-20260110
REGION=us-central1
REPO=cloud-run-source-deploy          # standard Cloud Run AR repo (push-able by AR-writer)
SERVICE=agentbbs-web
SA=darwin-nightly@cognitum-20260110.iam.gserviceaccount.com
GATEWAY=https://apicompletions-63rzcdswba-uc.a.run.app   # live meta-llm gateway (issue #6)
TAG="$(git rev-parse --short HEAD)"
IMAGE="${REGION}-docker.pkg.dev/${PROJECT}/${REPO}/${SERVICE}:${TAG}"
: "${AGENTBBS_NFS_LOCATION:?set Filestore IP:/share for durable redb storage}"
: "${AGENTBBS_ADMIN_CONSOLE_URL:?set trusted Comms control-plane URL; direct browser mutations cannot hold the HMAC key}"

ROTATION_ARGS=()
if [[ -n "${AGENTBBS_RESULTS_PREVIOUS_SIGNING_KEY_ID:-}" || -n "${AGENTBBS_RESULTS_PREVIOUS_SECRET_VERSION:-}" ]]; then
  : "${AGENTBBS_RESULTS_PREVIOUS_SIGNING_KEY_ID:?set both previous kid and secret version, or neither}"
  : "${AGENTBBS_RESULTS_PREVIOUS_SECRET_VERSION:?set both previous kid and secret version, or neither}"
  ROTATION_ARGS+=(
    "--update-secrets=AGENTBBS_RESULTS_PREVIOUS_SIGNING_SECRET=AGENTBBS_RESULTS_SIGNING_SECRET:${AGENTBBS_RESULTS_PREVIOUS_SECRET_VERSION}"
    "--update-env-vars=AGENTBBS_RESULTS_PREVIOUS_SIGNING_KEY_ID=${AGENTBBS_RESULTS_PREVIOUS_SIGNING_KEY_ID}"
  )
fi

echo "==> building $IMAGE"
gcloud auth configure-docker "${REGION}-docker.pkg.dev" -q
docker build -f deploy/Dockerfile -t "$IMAGE" .
docker push "$IMAGE"

echo "==> deploying Cloud Run service $SERVICE"
# AGENTBBS_LLM_BASE_URL ends in /v1 (chat_completions_url appends /chat/completions);
# AGENTBBS_PODS_BASE_URL is the root (pods_spawn_url appends /v1/pods/spawn).
gcloud run deploy "$SERVICE" \
  --project="$PROJECT" --region="$REGION" --image="$IMAGE" \
  --service-account="$SA" --allow-unauthenticated --port=8080 \
  --add-volume="name=agentbbs-data,type=nfs,location=${AGENTBBS_NFS_LOCATION}" \
  --add-volume-mount="volume=agentbbs-data,mount-path=/data" \
  --cpu=1 --memory=512Mi --max-instances=1 --min-instances=1 \
  --set-secrets="AGENTBBS_COGNITUM_KEY=AGENTBBS_COGNITUM_KEY:latest,AGENTBBS_ADMIN_ACTION_SECRET=AGENTBBS_ADMIN_ACTION_SECRET:latest,AGENTBBS_RESULTS_SIGNING_SECRET=AGENTBBS_RESULTS_SIGNING_SECRET:latest" \
  --set-env-vars="AGENTBBS_ENV=production,AGENTBBS_DB_PATH=/data/agentbbs.redb,AGENTBBS_ADMIN_ACTION_AUDIENCE=agentbbs-web,AGENTBBS_ADMIN_CONSOLE_URL=${AGENTBBS_ADMIN_CONSOLE_URL},AGENTBBS_RESULTS_ACCOUNT_ID=${AGENTBBS_RESULTS_ACCOUNT_ID:?set expected meta-llm account},AGENTBBS_RESULTS_SIGNING_KEY_ID=${AGENTBBS_RESULTS_SIGNING_KEY_ID:?set current kid},AGENTBBS_PODS_BASE_URL=${GATEWAY},AGENTBBS_LLM_BASE_URL=${GATEWAY}/v1,AGENTBBS_PODS_KEY_ENV=AGENTBBS_COGNITUM_KEY,AGENTBBS_LLM_KEY_ENV=AGENTBBS_COGNITUM_KEY,AGENTBBS_MODEL=cognitum-auto,AGENTBBS_LLM_DAILY_MAX=500,RUST_LOG=info" \
  "${ROTATION_ARGS[@]}"

URL="$(gcloud run services describe "$SERVICE" --project="$PROJECT" --region="$REGION" --format='value(status.url)')"
echo "==> deployed: $URL"
if curl -fsS "$URL/" >/dev/null; then echo "healthcheck OK ($URL/)"; else echo "healthcheck FAILED"; exit 1; fi
echo "$URL"
