#!/bin/sh
# mirrord installer
#             _                        _ 
#   _ __ ___ (_)_ __ _ __ ___  _ __ __| |
#  | '_ ` _ \| | '__| '__/ _ \| '__/ _` |
#  | | | | | | | |  | | | (_) | | | (_| |
#  |_| |_| |_|_|_|  |_|  \___/|_|  \__,_|
#
# Usage:
#   curl -fsSL https://github.com/metalbear-co/mirrord/raw/latest/scripts/install.sh | sh
#
# Written for POSIX sh so it runs under any /bin/sh, including dash on
# Debian/Ubuntu. Avoid bashisms ([[ ]], function, $OSTYPE, arrays, local).
set -e

file_issue_prompt() {
  echo "If you wish us to support your platform, please file an issue"
  echo "https://github.com/metalbear-co/mirrord/issues/new"
  exit 1
}

get_latest_version() {
  curl -fsSL https://github.com/metalbear-co/mirrord/raw/latest/Cargo.toml | grep version | head -n 1 | cut -d' ' -f3 | tr -d '"'
}

copy() {
  case ":$PATH:" in
    *":$HOME/.local/bin:"*)
      if [ ! -d "$HOME/.local/bin" ]; then
        mkdir -p "$HOME/.local/bin"
      fi
      mv /tmp/mirrord/mirrord "$HOME/.local/bin/mirrord"
      ;;
    *)
      # Try without sudo first, run with sudo only if mv failed without it.
      mv /tmp/mirrord/mirrord /usr/local/bin/mirrord || (
        echo "Cannot write to installation target directory as current user, writing as root."
        sudo mv /tmp/mirrord/mirrord /usr/local/bin/mirrord
      )
      ;;
  esac
}

# This function decides what version will be installed based on the following priority:
# 1. Environment variable `VERSION` is set.
# 2. Command line argument is passed.
# 3. Latest available on GitHub
get_version() {
  if [ -z "$VERSION" ]; then
      if [ -n "$1" ]; then
          VERSION="$1"
      else
          VERSION=$(get_latest_version)
      fi
  fi
  echo "$VERSION"
}

install() {
  version=$(get_version "$1")
  echo "Installing version $version"
  case "$(uname -s)" in
    Linux)
      ARCH=$(uname -m)
      OS="linux"
      if [ "$ARCH" != "x86_64" ] && [ "$ARCH" != "aarch64" ]; then
          echo "mirrord is only available for linux x86_64/aarch64 architecture"
          file_issue_prompt
      fi
      ;;
    Darwin)
      ARCH="universal"
      OS="mac"
      ;;
    *)
      echo "mirrord isn't supported for your platform - $(uname -s)"
      file_issue_prompt
      ;;
  esac
  mkdir -p /tmp/mirrord
  curl -o /tmp/mirrord/mirrord -fsSL "https://github.com/metalbear-co/mirrord/releases/download/$version/mirrord_${OS}_${ARCH}"
  chmod +x /tmp/mirrord/mirrord
  copy
  echo "mirrord installed! Have fun! For feedback and support, join our Slack: https://metalbear.co/slack , open an issue or discussion on our GitHub: https://github.com/metalbear-co/mirrord/ or send us an email at hi@metalbear.co"
}


install "$1"
