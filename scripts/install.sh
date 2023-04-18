#!/bin/bash
# mirrord installer
#             _                        _ 
#   _ __ ___ (_)_ __ _ __ ___  _ __ __| |
#  | '_ ` _ \| | '__| '__/ _ \| '__/ _` |
#  | | | | | | | |  | | | (_) | | | (_| |
#  |_| |_| |_|_|_|  |_|  \___/|_|  \__,_|
#
# Usage:
#   curl -fsSL https://github.com/metalbear-co/mirrord/raw/latest/scripts/install.sh | sh
set -e

file_issue_prompt() {
  echo "If you wish us to support your platform, please file an issue"
  echo "https://github.com/metalbear-co/mirrord/issues/new"
  exit 1
}

get_latest_version() {
  local res=$(curl -fsSL https://github.com/metalbear-co/mirrord/raw/latest/Cargo.toml | grep -m 1 version | cut -d' ' -f3 | tr -d '\"')
  echo $res
}

copy() {
  if [[ ":$PATH:" == *":$HOME/.local/bin:"* ]]; then
      if [ ! -d "$HOME/.local/bin" ]; then
        mkdir -p "$HOME/.local/bin"
      fi
      mv /tmp/mirrord "$HOME/.local/bin/mirrord"
  else
      echo "installation target directory is write protected, run as root to override"
      sudo mv /tmp/mirrord /usr/local/bin/mirrord
  fi
}

# This function decides what version will be installed based on the following priority:
# 1. Environment variable `VERSION` is set.
# 2. Command line argument is passed.
# 3. Latest available on GitHub
function get_version() {
  if [[ -z "$VERSION" ]]; then
      if [[ -n "$1" ]]; then
          VERSION="$1"
      else
          VERSION=$(get_latest_version)
      fi
  fi
  echo $VERSION
}

function install() {
  local version=$(get_version $1);
  echo "Installing version $version"
  if [[ "$OSTYPE" == "linux"* ]]; then
      ARCH=$(uname -m);
      OS="linux";
      if [[ "$ARCH" != "x86_64" && "$ARCH" != "aarch64" ]]; then
          echo "mirrord is only available for linux x86_64/aarch64 architecture"
          file_issue_prompt
          exit 1
      fi
  elif [[ "$OSTYPE" == "darwin"* ]]; then
      ARCH="universal";
      OS="mac";
  else
      echo "mirrord isn't supported for your platform - $OSTYPE"
      file_issue_prompt
      exit 1
  fi
  curl -o /tmp/mirrord -fsSL https://github.com/metalbear-co/mirrord/releases/download/$version/mirrord_$OS\_$ARCH
  chmod +x /tmp/mirrord
  copy
  echo "mirrord installed! Have fun! Join our discord server for help: https://discord.gg/metalbear"
  }


install $1
