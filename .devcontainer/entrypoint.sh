#!/usr/bin/env bash
set -euo pipefail

mkdir -p /var/run/sshd
exec /usr/sbin/sshd -D -e
