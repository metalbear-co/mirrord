#!/bin/sh
# This script builds a fat binary for Mac OS X.
# Output will be in target/universal-apple-darwin/debug/mirrord
# Any arguments provided to this script are passed to `cargo build`.
# If compilation fails, try running:
# rustup target add --toolchain nightly x86_64-apple-darwin

set -e

# build layer
cargo build "$@" -p mirrord-layer --target=x86_64-apple-darwin
cargo build "$@" -p mirrord-layer --target=aarch64-apple-darwin

# sign layer dylibs
codesign -f -s - target/aarch64-apple-darwin/debug/libmirrord_layer.dylib
codesign -f -s - target/x86_64-apple-darwin/debug/libmirrord_layer.dylib

# create dir to put universal binaries in
mkdir -p target/universal-apple-darwin/debug

# create shim to always load arm64 and sign it
clang -arch arm64e -dynamiclib -o target/universal-apple-darwin/debug/shim.dylib mirrord/layer/shim.c
codesign -f -s - target/universal-apple-darwin/debug/shim.dylib

# Create universal binary for mirrord-layer from x86 and shim, as well as arm64
lipo -create -output target/universal-apple-darwin/debug/libmirrord_layer.dylib target/x86_64-apple-darwin/debug/libmirrord_layer.dylib target/universal-apple-darwin/debug/shim.dylib target/aarch64-apple-darwin/debug/libmirrord_layer.dylib
codesign -f -s - target/universal-apple-darwin/debug/libmirrord_layer.dylib

# build mirrord package - aarch64-apple-darwin target requires MIRRORD_LAYER_FILE_MACOS_ARM64 var
MIRRORD_LAYER_FILE_MACOS_ARM64=../../../target/aarch64-apple-darwin/debug/libmirrord_layer.dylib MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build "$@" -p mirrord --target=aarch64-apple-darwin
MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build "$@" -p mirrord --target=x86_64-apple-darwin

# create universal binary for mirrord and sign
lipo -create -output target/universal-apple-darwin/debug/mirrord target/aarch64-apple-darwin/debug/mirrord target/x86_64-apple-darwin/debug/mirrord
codesign -f -s - target/universal-apple-darwin/debug/mirrord

