#!/usr/bin/env bash
# Builds the Go, C and Rust test apps and the Rust test binaries for the mirrord E2E suite.
# Run from the repo root. The e2e image installs the Go SDKs under /usr/local/go-versions;
# outside it the single `go` on PATH is used.
#
# Pass --apps-only to build just the test apps, skipping the E2E test binaries.

set -euo pipefail

group_open=""

group() {
    endgroup
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::group::$1"
        group_open=1
    else
        echo ">>> $1"
    fi
}

endgroup() {
    if [ -n "$group_open" ]; then
        echo "::endgroup::"
        group_open=""
    fi
}

trap endgroup EXIT

apps_only=false

case "${1-}" in
    --apps-only) apps_only=true ;;
    "") ;;
    *) 1>&2 echo "USAGE: $0 [--apps-only]"; exit 1 ;;
esac

if [ -d /usr/local/go-versions ]; then
    for sdk in /usr/local/go-versions/*/; do
        minor="$(basename "$sdk")"
        label="${minor#1.}"
        group "Building Go test apps with Go ${minor}"
        PATH="${sdk}bin:${PATH}" scripts/build_go_apps.sh "$label"
    done
else
    label="$(go env GOVERSION | sed -E 's/^go1\.([0-9]+).*/\1/')"
    group "Building Go test apps with $(go version)"
    scripts/build_go_apps.sh "$label"
fi

group "Building C test apps"
scripts/build_c_apps.sh

group "Installing Node test app dependencies"
pnpm --filter mirrord-frontends install --frozen-lockfile

group "Building Rust test apps"
for manifest in tests/*/Cargo.toml; do
    cargo_dir="$(dirname "$manifest")"
    echo "Building Rust test app in ${cargo_dir}"
    (cd "$cargo_dir" && cargo build)
done

endgroup

if [ "$apps_only" = true ]; then
    exit 0
fi

group "Compiling Rust E2E test binaries"
cargo test --no-run --package mirrord-tests
