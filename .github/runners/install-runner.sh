#!/bin/bash
# GitHub Actions Runner Installation Script
# Run this script on each GCP VM instance to install and configure the runner
#
# Usage:
#   GITHUB_TOKEN=<token> RUNNER_NAME=<name> RUNNER_LABELS=<labels> ./install-runner.sh
#
# Example:
#   GITHUB_TOKEN=ghp_xxx RUNNER_NAME=linux-docker RUNNER_LABELS=self-hosted,linux,x64,docker ./install-runner.sh

set -euo pipefail

# Configuration
GITHUB_REPO="${GITHUB_REPO:-metalbear-co/mirrord}"
RUNNER_USER="${RUNNER_USER:-runner}"
RUNNER_DIR="/home/${RUNNER_USER}/actions-runner"
CACHE_DIR="/mnt/cache"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
  echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
  echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
  echo -e "${RED}[ERROR]${NC} $1"
}

# Validate required environment variables
if [ -z "${GITHUB_TOKEN:-}" ]; then
  log_error "GITHUB_TOKEN environment variable is required"
  log_info "Get a token from: https://github.com/settings/tokens/new?scopes=repo,admin:org"
  exit 1
fi

if [ -z "${RUNNER_NAME:-}" ]; then
  log_error "RUNNER_NAME environment variable is required"
  log_info "Example: RUNNER_NAME=linux-docker"
  exit 1
fi

RUNNER_LABELS="${RUNNER_LABELS:-self-hosted,linux,x64}"
if [[ "${SPOT_RUNNER:-false}" == "true" ]]; then
  if [[ "${RUNNER_LABELS}" != *spot* ]]; then
    RUNNER_LABELS="${RUNNER_LABELS},spot"
  fi
fi

# Detect architecture
ARCH=$(uname -m)
if [ "$ARCH" = "x86_64" ]; then
  RUNNER_ARCH="x64"
elif [ "$ARCH" = "aarch64" ]; then
  RUNNER_ARCH="arm64"
else
  log_error "Unsupported architecture: $ARCH"
  exit 1
fi

log_info "Installing GitHub Actions runner..."
log_info "Repository: ${GITHUB_REPO}"
log_info "Runner name: ${RUNNER_NAME}"
log_info "Runner labels: ${RUNNER_LABELS}"
log_info "Architecture: ${RUNNER_ARCH}"

# Create runner user if it doesn't exist
if ! id -u "${RUNNER_USER}" &>/dev/null; then
  log_info "Creating user: ${RUNNER_USER}"
  useradd -m -s /bin/bash "${RUNNER_USER}"
  usermod -aG docker "${RUNNER_USER}"
else
  log_info "User ${RUNNER_USER} already exists"
fi

# Install dependencies
log_info "Installing dependencies..."
apt-get update
apt-get install -y curl wget git build-essential libssl-dev pkg-config

# Install Rust (if not already installed)
if ! command -v rustc &>/dev/null; then
  log_info "Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

# Install Node.js (if not already installed)
if ! command -v node &>/dev/null; then
  log_info "Installing Node.js..."
  curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
  apt-get install -y nodejs
fi

# Install Python (if not already installed)
if ! command -v python3 &>/dev/null; then
  log_info "Installing Python..."
  apt-get install -y python3 python3-pip python3-venv
fi

# Download and install runner
log_info "Downloading GitHub Actions runner..."
cd /tmp
RUNNER_VERSION=$(curl -s https://api.github.com/repos/actions/runner/releases/latest | grep tag_name | cut -d '"' -f 4 | sed 's/v//')
RUNNER_URL="https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-${RUNNER_ARCH}-${RUNNER_VERSION}.tar.gz"

curl -o actions-runner.tar.gz -L "${RUNNER_URL}"
tar xzf actions-runner.tar.gz

# Move to runner directory
mkdir -p "${RUNNER_DIR}"
mv actions-runner/* "${RUNNER_DIR}/"
rm -rf actions-runner actions-runner.tar.gz

# Configure runner
log_info "Configuring runner..."
cd "${RUNNER_DIR}"

# Get registration token
REGISTRATION_TOKEN=$(curl -X POST \
  -H "Authorization: token ${GITHUB_TOKEN}" \
  -H "Accept: application/vnd.github.v3+json" \
  "https://api.github.com/repos/${GITHUB_REPO}/actions/runners/registration-token" | \
  grep -o '"token":"[^"]*' | cut -d'"' -f4)

# Configure runner
sudo -u "${RUNNER_USER}" ./config.sh \
  --url "https://github.com/${GITHUB_REPO}" \
  --token "${REGISTRATION_TOKEN}" \
  --name "${RUNNER_NAME}" \
  --labels "${RUNNER_LABELS}" \
  --work "_work" \
  --replace

# Set up cache directories
log_info "Setting up cache directories..."
mkdir -p "${CACHE_DIR}/rust"
mkdir -p "${CACHE_DIR}/docker"
mkdir -p "${CACHE_DIR}/cargo-registry"
chown -R "${RUNNER_USER}:${RUNNER_USER}" "${CACHE_DIR}"
chmod -R 755 "${CACHE_DIR}"

# Create environment file for cache paths
cat > "${RUNNER_DIR}/.env" <<EOF
CARGO_TARGET_DIR=${CACHE_DIR}/rust/target
CARGO_HOME=${CACHE_DIR}/cargo-registry
DOCKER_BUILDKIT=1
DOCKER_CACHE_DIR=${CACHE_DIR}/docker
EOF
chown "${RUNNER_USER}:${RUNNER_USER}" "${RUNNER_DIR}/.env"

# Install and start service
log_info "Installing runner service..."
./svc.sh install "${RUNNER_USER}"
./svc.sh start

log_info "Runner installation complete!"
log_info ""
log_info "Runner status:"
systemctl status actions.runner."${GITHUB_REPO//\//.}"."${RUNNER_NAME}".service --no-pager || true

log_info ""
log_info "To check logs:"
log_info "  journalctl -u actions.runner.${GITHUB_REPO//\//.}.${RUNNER_NAME}.service -f"




