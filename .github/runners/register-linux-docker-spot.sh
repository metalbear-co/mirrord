#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"${SCRIPT_DIR}/register-spot-runner.sh" "linux-docker-spot" "self-hosted,linux,x64,docker"




