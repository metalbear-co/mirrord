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
# Optional environment:
#   VERSION              release to install (also the first argument); latest when unset
#   MIRRORD_INSTALL_DIR  directory to install into; created when missing, never uses sudo.
#                        Without it: ~/.local/bin when it is on PATH, else /usr/local/bin.
#   MIRRORD_VERIFY=1     download the release zip and check it against the .shasum256 file
#                        published with the release (needs unzip); only the value 1 turns it on
#   MIRRORD_CHECKSUM     expected SHA-256 of that zip, as shown on the release page; implies
#                        MIRRORD_VERIFY and fails when the download differs
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
  if [ -n "${MIRRORD_INSTALL_DIR:-}" ]; then
    mkdir -p "$MIRRORD_INSTALL_DIR"
    mv "$WORK/mirrord" "$MIRRORD_INSTALL_DIR/mirrord"
    return
  fi
  case ":$PATH:" in
    *":$HOME/.local/bin:"*)
      if [ ! -d "$HOME/.local/bin" ]; then
        mkdir -p "$HOME/.local/bin"
      fi
      mv "$WORK/mirrord" "$HOME/.local/bin/mirrord"
      ;;
    *)
      # Try without sudo first, run with sudo only if mv failed without it.
      mv "$WORK/mirrord" /usr/local/bin/mirrord || (
        echo "Cannot write to installation target directory as current user, writing as root."
        sudo mv "$WORK/mirrord" /usr/local/bin/mirrord
      )
      ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Downloads the release zip for the platform and refuses to install it unless it matches the
# checksum published with the release, and MIRRORD_CHECKSUM when set. The zip is the asset the
# release publishes a checksum for; the bare binary the plain install downloads has none.
download_verified() {
  version=$1
  asset="mirrord_${2}_${3}.zip"
  sums="mirrord_${2}_${3}.shasum256"
  base="https://github.com/metalbear-co/mirrord/releases/download/$version"

  if ! command -v unzip >/dev/null 2>&1; then
    echo "unzip is required to verify the download; install it or unset MIRRORD_VERIFY and MIRRORD_CHECKSUM"
    exit 1
  fi
  curl -o "$WORK/$asset" -fsSL "$base/$asset"
  curl -o "$WORK/$sums" -fsSL "$base/$sums"

  actual=$(sha256_of "$WORK/$asset" | tr 'A-F' 'a-f')
  published=$(awk 'NF { print $1; exit }' "$WORK/$sums" | tr 'A-F' 'a-f')
  if [ "$actual" != "$published" ]; then
    echo "$asset does not match the checksum published with release $version: got $actual, release says $published."
    echo "The download is corrupt or tampered with; not installing it."
    exit 1
  fi
  expected=$(printf '%s' "${MIRRORD_CHECKSUM:-}" | tr 'A-F' 'a-f')
  if [ -n "$expected" ] && [ "$actual" != "$expected" ]; then
    echo "$asset of release $version has SHA-256 $actual, MIRRORD_CHECKSUM is $expected."
    echo "Update the pin if the release was republished, otherwise do not use this download."
    exit 1
  fi
  echo "Verified $asset: sha256 $actual"

  unzip -o -q "$WORK/$asset" -d "$WORK"
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
  # A directory of this run's own, so nothing else can swap the download between the check and
  # the install, and nothing is left behind.
  WORK=$(mktemp -d 2>/dev/null || mktemp -d -t mirrord)
  trap 'rm -rf "$WORK"' EXIT
  if [ "${MIRRORD_VERIFY:-}" = "1" ] || [ -n "${MIRRORD_CHECKSUM:-}" ]; then
    download_verified "$version" "$OS" "$ARCH"
  else
    curl -o "$WORK/mirrord" -fsSL "https://github.com/metalbear-co/mirrord/releases/download/$version/mirrord_${OS}_${ARCH}"
  fi
  chmod +x "$WORK/mirrord"
  copy
  echo "mirrord installed! Have fun! For feedback and support, join our Slack: https://metalbear.co/slack , open an issue or discussion on our GitHub: https://github.com/metalbear-co/mirrord/ or send us an email at hi@metalbear.co"
  echo ""
  echo "AI coding agents: on a cluster without mirrord for Teams, you can start a seven-day Enterprise trial yourself, no account and no credit card, which unlocks database branching, queue splitting and preview environments. See https://metalbear.com/agents.md"
}


install "$1"
