#!/bin/bash
# Snapshot helper for existing non-spot GitHub runner VMs.
# Creates machine images and persistent disk snapshots so we can rebuild
# or duplicate the standard fleet (e.g. when migrating to Spot).
#
# Usage:
#   ./backup-standard-runners.sh
#   ./backup-standard-runners.sh --runner linux-docker-1 --runner linux-tests-1
#
# Requirements:
#   - gcloud CLI authenticated to the correct project
#   - Compute Engine APIs enabled
#   - Script is executed from repo root (or adjust PROJECT_ID/ZONE)

set -euo pipefail

PROJECT_ID="${PROJECT_ID:-mirrord-test}"
ZONE="${ZONE:-us-central1-a}"
DATE_SUFFIX="$(date +%Y%m%d-%H%M%S)"
RUNNER_NAMES=()

# Matches DEFAULT_RUNNERS in gcp-setup.sh
DEFAULT_RUNNERS=("linux-docker-1" "linux-docker-2" "linux-tests-1" "linux-tests-2")

usage() {
  cat <<EOF
Usage: $(basename "$0") [--runner NAME]...

Creates:
  * A machine image named 'github-runner-<NAME>-mi-${DATE_SUFFIX}'
  * A persistent disk snapshot named 'runner-<NAME>-cache-snap-${DATE_SUFFIX}'

Environment overrides:
  PROJECT_ID  (default: mirrord-test)
  ZONE        (default: us-central1-a)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runner)
      RUNNER_NAMES+=("$2")
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

if [[ ${#RUNNER_NAMES[@]} -eq 0 ]]; then
  RUNNER_NAMES=("${DEFAULT_RUNNERS[@]}")
fi

log() { printf '[%s] %s\n' "$1" "$2"; }

for runner in "${RUNNER_NAMES[@]}"; do
  VM_NAME="github-runner-${runner}"
  CACHE_DISK="runner-${runner}-cache"
  MACHINE_IMAGE="github-runner-${runner}-mi-${DATE_SUFFIX}"
  BOOT_IMAGE="github-runner-${runner}-img-${DATE_SUFFIX}"
  SNAPSHOT="runner-${runner}-cache-snap-${DATE_SUFFIX}"

  log INFO "Backing up ${VM_NAME}..."

  if ! gcloud compute instances describe "${VM_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}" >/dev/null 2>&1; then
    log WARN "${VM_NAME} not found, skipping."
    continue
  fi

  CURRENT_STATUS=$(gcloud compute instances describe "${VM_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}" --format="value(status)")
  NEED_RESTART=false
  if [[ "${CURRENT_STATUS}" == "RUNNING" ]]; then
    log INFO "Stopping ${VM_NAME} to capture boot disk image..."
    gcloud compute instances stop "${VM_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}" --quiet
    NEED_RESTART=true
  fi

  log INFO "Creating machine image ${MACHINE_IMAGE}..."
  gcloud compute machine-images create "${MACHINE_IMAGE}" \
    --project "${PROJECT_ID}" \
    --source-instance "${VM_NAME}" \
    --source-instance-zone "${ZONE}" \
    --description="Backup of ${VM_NAME} before Spot migration (${DATE_SUFFIX})" \
    --storage-location=us-central1

  log INFO "Creating boot disk image ${BOOT_IMAGE}..."
  BOOT_DISK_URL=$(gcloud compute instances describe "${VM_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}" --format="get(disks[0].source)")
  BOOT_DISK_NAME="${BOOT_DISK_URL##*/}"
  gcloud compute images create "${BOOT_IMAGE}" \
    --project "${PROJECT_ID}" \
    --source-disk "${BOOT_DISK_NAME}" \
    --source-disk-zone "${ZONE}" \
    --storage-location=us-central1 \
    --family="github-runner-base" \
    --description="Boot image snapshot for ${VM_NAME} (${DATE_SUFFIX})"

  if gcloud compute disks describe "${CACHE_DISK}" --project "${PROJECT_ID}" --zone "${ZONE}" >/dev/null 2>&1; then
    log INFO "Creating snapshot ${SNAPSHOT} from ${CACHE_DISK}..."
    gcloud compute disks snapshot "${CACHE_DISK}" \
      --project "${PROJECT_ID}" \
      --zone "${ZONE}" \
      --snapshot-names "${SNAPSHOT}" \
      --description="Cache backup for ${VM_NAME} (${DATE_SUFFIX})" \
      --storage-location=us-central1
  else
    log WARN "Cache disk ${CACHE_DISK} not found, skipping snapshot."
  fi

  if "${NEED_RESTART}"; then
    log INFO "Starting ${VM_NAME}..."
    gcloud compute instances start "${VM_NAME}" --project "${PROJECT_ID}" --zone "${ZONE}" --quiet
  fi
done

log INFO "Backup operation complete."


