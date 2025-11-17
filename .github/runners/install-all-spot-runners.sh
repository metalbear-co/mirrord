#!/bin/bash
# Helper to register all Spot VMs with GitHub Actions using install-runner.sh.
#
# Usage:
#   GITHUB_TOKEN=<registration token> ./install-all-spot-runners.sh
#   GITHUB_TOKEN=<token> ./install-all-spot-runners.sh --runner linux-docker-spot
#
# Requirements:
#   - gcloud CLI authenticated to the correct project
#   - GITHUB_TOKEN exports a repo registration token (repo/admin:org scope)
#   - VMs already created by gcp-setup.sh with names github-runner-<runner>

set -euo pipefail

PROJECT_ID="${PROJECT_ID:-mirrord-test}"
ZONE="${ZONE:-us-central1-a}"
GITHUB_TOKEN="${GITHUB_TOKEN:-}"

DEFAULT_RUNNERS=(
  "linux-docker-spot:self-hosted,linux,x64,docker"
  "linux-docker-2-spot:self-hosted,linux,x64,docker"
  "linux-tests-1-spot:self-hosted,linux,x64,tests"
  "linux-tests-2-spot:self-hosted,linux,x64,tests"
)

RUNNER_LIST=()

usage() {
  cat <<EOF
Usage: GITHUB_TOKEN=<token> $(basename "$0") [--runner NAME:labels]...

Registers each Spot VM by SSHing in and invoking install-runner.sh with:
  SPOT_RUNNER=true RUNNER_NAME=<NAME> RUNNER_LABELS=<labels>

Options:
  --runner NAME:labels   Override/add runner in format name:labels (repeatable)
  -h, --help             Show this help message

Environment:
  PROJECT_ID             GCP project (default: mirrord-test)
  ZONE                   GCP zone (default: us-central1-a)
  GITHUB_TOKEN           GitHub registration token (required)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runner)
      RUNNER_LIST+=("$2")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "${GITHUB_TOKEN}" ]]; then
  echo "GITHUB_TOKEN environment variable is required." >&2
  exit 1
fi

if [[ ${#RUNNER_LIST[@]} -eq 0 ]]; then
  RUNNER_LIST=("${DEFAULT_RUNNERS[@]}")
fi

for entry in "${RUNNER_LIST[@]}"; do
  if [[ "${entry}" != *:* ]]; then
    echo "Invalid entry '${entry}'. Expected format name:labels." >&2
    exit 1
  fi
done

for entry in "${RUNNER_LIST[@]}"; do
  IFS=':' read -r runner_name runner_labels <<< "${entry}"
  VM_NAME="github-runner-${runner_name}"

  echo "==> Configuring ${VM_NAME} (${runner_labels})"

  if ! gcloud compute instances describe "${VM_NAME}" \
    --project "${PROJECT_ID}" \
    --zone "${ZONE}" >/dev/null 2>&1; then
    echo "    VM ${VM_NAME} not found; skipping."
    continue
  fi

  gcloud compute ssh "${VM_NAME}" \
    --project "${PROJECT_ID}" \
    --zone "${ZONE}" \
    --command "
      set -euo pipefail
      sudo -H -u runner bash -lc '
        set -euo pipefail
        if [ ! -d /home/runner/mirrord/.github/runners ]; then
          git clone https://github.com/metalbear-co/mirrord.git /home/runner/mirrord || true
        fi
        cd /home/runner/mirrord/.github/runners
        chmod +x install-runner.sh
        SPOT_RUNNER=true \
        GITHUB_TOKEN=\"${GITHUB_TOKEN}\" \
        RUNNER_NAME=\"${runner_name}\" \
        RUNNER_LABELS=\"${runner_labels}\" \
        ./install-runner.sh
      '
    "
done

echo "All specified runners processed."


