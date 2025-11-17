#!/bin/bash
# GCP Setup Script for GitHub Actions Self-Hosted Runners
# Project: mirrord
#
# This script creates GCP resources for self-hosted runners:
# - VPC network (if needed)
# - Firewall rules
# - VM instances for Linux runners
# - Persistent disks for caching
#
# Prerequisites:
# - gcloud CLI installed and authenticated
# - GCP project "mirrord" exists
# - Appropriate permissions (Compute Admin)

set -euo pipefail

PROJECT_ID="mirrord-test"
REGION="us-central1"
ZONE="us-central1-a"
NETWORK_NAME="github-runners-network"
SUBNET_NAME="github-runners-subnet"
FIREWALL_RULE="github-runners-allow-egress"
SSH_FIREWALL_RULE="github-runners-allow-ssh"
UBUNTU_IMAGE_FAMILY="ubuntu-2204-lts"   # Stable LTS image family
UBUNTU_IMAGE_PROJECT="ubuntu-os-cloud"
CUSTOM_IMAGE=""

# Runner configurations (name:machine-type)
DEFAULT_RUNNERS=(
  "linux-docker-1:n1-standard-8"  # Docker runner #1
  "linux-docker-2:n1-standard-8"  # Docker runner #2
  "linux-tests-1:n1-standard-4"   # Tests runner #1
  "linux-tests-2:n1-standard-4"   # Tests runner #2
)

PROVISIONING_MODEL_FLAG=(--provisioning-model=STANDARD)
MAINTENANCE_POLICY_FLAG=(--maintenance-policy=MIGRATE)
SPOT_TERMINATION_FLAG=()
SPOT_MODE=false

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STARTUP_SCRIPT="${SCRIPT_DIR}/startup-script.sh"
RUNNER_OVERRIDES=()

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
  echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
  echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
  echo -e "${RED}[ERROR]${NC} $1"
}

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Without options, creates the default runner pool:
  linux-docker (n1-standard-8)
  linux-tests  (n1-standard-4)

Options:
  --create-runner NAME MACHINE_TYPE   Create only the specified runner (can be repeated)
  --boot-image IMAGE_NAME             Use custom boot disk image instead of Ubuntu family
  --use-spot                          Provision runners as Spot VMs (preemptible)
  -h, --help                          Show this help message

Examples:
  $(basename "$0") --create-runner linux-docker-2 n1-standard-8
  $(basename "$0") --create-runner linux-docker-2 n1-standard-8 \\
                   --create-runner linux-tests-2 n1-standard-4
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --create-runner)
      if [[ $# -lt 3 ]]; then
        echo "Error: --create-runner requires NAME and MACHINE_TYPE" >&2
        usage
        exit 1
      fi
      RUNNER_OVERRIDES+=("$2:$3")
      shift 3
      ;;
    --use-spot)
      SPOT_MODE=true
      PROVISIONING_MODEL_FLAG=(--provisioning-model=SPOT)
      MAINTENANCE_POLICY_FLAG=(--maintenance-policy=TERMINATE)
      SPOT_TERMINATION_FLAG=(--instance-termination-action=STOP)
      shift
      ;;
    --boot-image)
      CUSTOM_IMAGE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: Unknown option $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [ ${#RUNNER_OVERRIDES[@]} -gt 0 ]; then
  RUNNER_LIST=("${RUNNER_OVERRIDES[@]}")
  echo "Using custom runner list:"
else
  RUNNER_LIST=("${DEFAULT_RUNNERS[@]}")
  echo "Using default runner list:"
fi
for runner_entry in "${RUNNER_LIST[@]}"; do
  IFS=':' read -r runner_name runner_type <<< "${runner_entry}"
  echo "  - ${runner_name} (${runner_type})"
done

if "${SPOT_MODE}"; then
  log_warn "Spot mode enabled: runners may be preempted with 30 seconds notice. Ensure workflows handle retries."
fi

# Set the GCP project
log_info "Setting GCP project to ${PROJECT_ID}..."
gcloud config set project "${PROJECT_ID}"

# Ensure startup script exists
if [ ! -f "${STARTUP_SCRIPT}" ]; then
  log_error "Startup script not found at ${STARTUP_SCRIPT}"
  exit 1
fi

# Enable required APIs
log_info "Enabling required GCP APIs..."
gcloud services enable compute.googleapis.com
gcloud services enable iam.googleapis.com

# Create VPC network (if it doesn't exist)
log_info "Creating VPC network..."
if gcloud compute networks describe "${NETWORK_NAME}" --project="${PROJECT_ID}" &>/dev/null; then
  log_warn "Network ${NETWORK_NAME} already exists"
else
  if gcloud compute networks create "${NETWORK_NAME}" \
    --project="${PROJECT_ID}" \
    --subnet-mode=custom \
    --bgp-routing-mode=regional; then
    log_info "Created network: ${NETWORK_NAME}"
  else
    log_warn "Failed to create network ${NETWORK_NAME} (it may already exist)"
  fi
fi

# Create subnet (if it doesn't exist)
log_info "Creating subnet..."
if gcloud compute networks subnets describe "${SUBNET_NAME}" \
  --network="${NETWORK_NAME}" \
  --project="${PROJECT_ID}" \
  --region="${REGION}" &>/dev/null; then
  log_warn "Subnet ${SUBNET_NAME} already exists"
else
  if gcloud compute networks subnets create "${SUBNET_NAME}" \
    --project="${PROJECT_ID}" \
    --network="${NETWORK_NAME}" \
    --region="${REGION}" \
    --range="10.0.0.0/24"; then
    log_info "Created subnet: ${SUBNET_NAME}"
  else
    log_warn "Failed to create subnet ${SUBNET_NAME} (it may already exist)"
  fi
fi

# Create firewall rule for egress (if it doesn't exist)
log_info "Creating firewall rule for egress..."
if gcloud compute firewall-rules describe "${FIREWALL_RULE}" \
  --project="${PROJECT_ID}" &>/dev/null; then
  log_warn "Firewall rule ${FIREWALL_RULE} already exists"
else
  if gcloud compute firewall-rules create "${FIREWALL_RULE}" \
    --project="${PROJECT_ID}" \
    --network="${NETWORK_NAME}" \
    --allow tcp,udp,icmp \
    --direction=EGRESS \
    --priority=1000 \
    --description="Allow egress for GitHub runners"; then
    log_info "Created firewall rule: ${FIREWALL_RULE}"
  else
    log_warn "Failed to create firewall rule ${FIREWALL_RULE} (it may already exist)"
  fi
fi

# Create firewall rule for SSH ingress (if it doesn't exist)
log_info "Creating firewall rule for SSH ingress..."
if gcloud compute firewall-rules describe "${SSH_FIREWALL_RULE}" \
  --project="${PROJECT_ID}" &>/dev/null; then
  log_warn "Firewall rule ${SSH_FIREWALL_RULE} already exists"
else
  if gcloud compute firewall-rules create "${SSH_FIREWALL_RULE}" \
    --project="${PROJECT_ID}" \
    --network="${NETWORK_NAME}" \
    --allow tcp:22 \
    --direction=INGRESS \
    --priority=1000 \
    --source-ranges="0.0.0.0/0" \
    --target-tags=github-runner \
    --description="Allow SSH access to GitHub runner instances"; then
    log_info "Created firewall rule: ${SSH_FIREWALL_RULE}"
  else
    log_warn "Failed to create firewall rule ${SSH_FIREWALL_RULE} (it may already exist)"
  fi
fi

# Create persistent disks for caching
log_info "Creating persistent disks for caching..."
for runner_entry in "${RUNNER_LIST[@]}"; do
  IFS=':' read -r runner_name _machine_type <<< "${runner_entry}"
  DISK_NAME="runner-${runner_name}-cache"
  if ! gcloud compute disks describe "${DISK_NAME}" \
    --project="${PROJECT_ID}" \
    --zone="${ZONE}" &>/dev/null; then
    gcloud compute disks create "${DISK_NAME}" \
      --project="${PROJECT_ID}" \
      --zone="${ZONE}" \
      --size=200GB \
      --type=pd-ssd \
      --description="Persistent cache disk for ${runner_name} runner"
    log_info "Created disk: ${DISK_NAME}"
  else
    log_warn "Disk ${DISK_NAME} already exists"
  fi
done

# Create VM instances
log_info "Creating VM instances..."
for runner_entry in "${RUNNER_LIST[@]}"; do
  IFS=':' read -r runner_name machine_type <<< "${runner_entry}"
  VM_NAME="github-runner-${runner_name}"
  MACHINE_TYPE="${machine_type}"
  DISK_NAME="runner-${runner_name}-cache"
  if [[ -n "${CUSTOM_IMAGE}" ]]; then
    BOOT_DISK_ARGS=(--create-disk=auto-delete=yes,boot=yes,device-name="${VM_NAME}",image="${CUSTOM_IMAGE}",size=50GB,type=pd-ssd)
  else
    BOOT_DISK_ARGS=(--create-disk=auto-delete=yes,boot=yes,device-name="${VM_NAME}",image-family="${UBUNTU_IMAGE_FAMILY}",image-project="${UBUNTU_IMAGE_PROJECT}",size=50GB,type=pd-ssd)
  fi
  
  if ! gcloud compute instances describe "${VM_NAME}" \
    --project="${PROJECT_ID}" \
    --zone="${ZONE}" &>/dev/null; then
    log_info "Creating VM: ${VM_NAME} (${MACHINE_TYPE})..."
    
    gcloud compute instances create "${VM_NAME}" \
      --project="${PROJECT_ID}" \
      --zone="${ZONE}" \
      --machine-type="${MACHINE_TYPE}" \
      --network-interface=network-tier=PREMIUM,subnet="${SUBNET_NAME}" \
      "${MAINTENANCE_POLICY_FLAG[@]}" \
      "${PROVISIONING_MODEL_FLAG[@]}" \
      "${SPOT_TERMINATION_FLAG[@]}" \
      --service-account="$(gcloud iam service-accounts list --filter="displayName:Compute Engine default service account" --format="value(email)")" \
      --scopes=https://www.googleapis.com/auth/cloud-platform \
      --tags=github-runner \
      "${BOOT_DISK_ARGS[@]}" \
      --disk=auto-delete=no,device-name="${DISK_NAME}",name="${DISK_NAME}",mode=rw \
      --metadata-from-file=startup-script="${STARTUP_SCRIPT}"
    
    log_info "Created VM: ${VM_NAME}"
  else
    log_warn "VM ${VM_NAME} already exists"
  fi
done

log_info "Setup complete!"
log_info ""
log_info "Next steps:"
log_info "1. SSH into each VM: gcloud compute ssh github-runner-<name> --zone=${ZONE}"
log_info "2. Run the runner installation script: .github/runners/install-runner.sh"
log_info "3. Update your CI workflow to use 'self-hosted' or 'self-hosted-<name>' labels"

