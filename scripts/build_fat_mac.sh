#!/bin/sh
# This script builds a fat binary for Mac OS X.
# Output will be in target/universal-apple-darwin/debug/mirrord
# Any arguments provided to this script are passed to `cargo build`.
# If compilation fails, try running:
# rustup target add --toolchain nightly x86_64-apple-darwin

set -e

./scripts/build_layer_mac.sh "$@"

# build mirrord package - both targets require MIRRORD_LAYER_FILE_MACOS_ARM64 var
MIRRORD_LAYER_FILE_MACOS_ARM64=../../../target/aarch64-apple-darwin/debug/libmirrord_layer.dylib MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build "$@" -p mirrord --target=aarch64-apple-darwin
MIRRORD_LAYER_FILE_MACOS_ARM64=../../../target/aarch64-apple-darwin/debug/libmirrord_layer.dylib MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build "$@" -p mirrord --target=x86_64-apple-darwin

# create universal binary for mirrord and sign
lipo -create -output target/universal-apple-darwin/debug/mirrord target/aarch64-apple-darwin/debug/mirrord target/x86_64-apple-darwin/debug/mirrord
codesign -f -s - target/universal-apple-darwin/debug/mirrord

