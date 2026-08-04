#!/usr/bin/env bash
set -euo pipefail

echo "Installing RTK..."

# Prefer the official repo install to avoid the crates.io name-collision issue.
# cargo install --git https://github.com/rtk-ai/rtk

# Make sure Cargo bin is on PATH for interactive shells.
if ! grep -q 'export PATH="$HOME/.cargo/bin:$PATH"' "$HOME/.bashrc"; then
  echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> "$HOME/.bashrc"
fi

export PATH="$HOME/.cargo/bin:$PATH"

echo "Verifying RTK..."
rtk --version
rtk gain

echo "Initializing RTK for Claude Code / agent hooks..."
# Non-interactive global setup inside the container.
rtk init -g --codex

echo "RTK setup complete."

# chmod 1777 /tmp