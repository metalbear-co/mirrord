#!/bin/bash
# Registers a single Spot VM runner with GitHub Actions.
#
# Usage:
#   GITHUB_TOKEN=<registration token> ./register-spot-runner.sh RUNNER_NAME RUNNER_LABELS
#   PROJECT_ID=mirrord-test ZONE=us-central1-a GITHUB_TOKEN=<token> ./register-spot-runner.sh linux-docker-spot self-hosted,linux,x64,docker
#
# Requirements:
#   - gcloud CLI installed and authenticated
#   - GITHUB_TOKEN environment variable set to a repository registration token
#   - Spot VM named github-runner-<RUNNER_NAME> already exists

set -euo pipefail

PROJECT_ID="${PROJECT_ID:-mirrord-test}"
ZONE="${ZONE:-us-central1-a}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ $# -lt 2 ]]; then
  echo "Usage: GITHUB_TOKEN=<token> $0 RUNNER_NAME RUNNER_LABELS" >&2
  exit 1
fi

RUNNER_NAME="$1"
RUNNER_LABELS="$2"
VM_NAME="github-runner-${RUNNER_NAME}"

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  echo "Error: GITHUB_TOKEN environment variable must be set to a GitHub registration token." >&2
  exit 1
fi

if ! gcloud compute instances describe "${VM_NAME}" \
  --project "${PROJECT_ID}" \
  --zone "${ZONE}" >/dev/null 2>&1; then
  echo "Error: VM ${VM_NAME} not found in project ${PROJECT_ID}, zone ${ZONE}." >&2
  exit 1
fi

echo "==> Registering ${VM_NAME} with labels [${RUNNER_LABELS}]"

REMOTE_SCRIPT="/tmp/install-runner.sh"

gcloud compute scp "${SCRIPT_DIR}/install-runner.sh" \
  "${VM_NAME}:${REMOTE_SCRIPT}" \
  --project "${PROJECT_ID}" \
  --zone "${ZONE}" >/dev/null

gcloud compute ssh "${VM_NAME}" \
  --project "${PROJECT_ID}" \
  --zone "${ZONE}" -- \
  sudo -H -u runner env \
    GITHUB_TOKEN="${GITHUB_TOKEN}" \
    RUNNER_NAME="${RUNNER_NAME}" \
    RUNNER_LABELS="${RUNNER_LABELS}" \
  bash -s <<'EOF'
set -euo pipefail

INSTALL_DIR="/home/runner/mirrord-runner"
INSTALL_SCRIPT="${INSTALL_DIR}/install-runner.sh"

mkdir -p "\${INSTALL_DIR}"

cp "${REMOTE_SCRIPT}" "\${INSTALL_SCRIPT}"
chmod +x "\${INSTALL_SCRIPT}"

SPOT_RUNNER=true \
GITHUB_TOKEN="\${GITHUB_TOKEN}" \
RUNNER_NAME="\${RUNNER_NAME}" \
RUNNER_LABELS="\${RUNNER_LABELS}" \
"\${INSTALL_SCRIPT}"
EOF

gcloud compute ssh "${VM_NAME}" \
  --project "${PROJECT_ID}" \
  --zone "${ZONE}" -- \
  sudo rm -f "${REMOTE_SCRIPT}" >/dev/null

echo "Registration complete for ${VM_NAME}."


