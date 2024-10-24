#!/bin/sh
# This script builds a fat binary for Mac OS X.
# Output will be in target/universal-apple-darwin/debug/mirrord
# If compilation fails, try running:
# rustup target add --toolchain nightly x86_64-apple-darwin

set -e

# build layer
cargo build -p mirrord-layer --target=x86_64-apple-darwin
cargo build -p mirrord-layer --target=aarch64-apple-darwin

# sign layer dylibs
codesign -f -s - target/aarch64-apple-darwin/debug/libmirrord_layer.dylib
codesign -f -s - target/x86_64-apple-darwin/debug/libmirrord_layer.dylib

# create dir to put universal binaries in
mkdir -p target/universal-apple-darwin/debug

# create shim to always load arm64 and put it in universal binary
clang -arch arm64e -dynamiclib -o target/universal-apple-darwin/debug/shim.dylib scripts/shim.c
lipo -create -output target/universal-apple-darwin/debug/libmirrord_layer.dylib target/x86_64-apple-darwin/debug/libmirrord_layer.dylib target/universal-apple-darwin/debug/shim.dylib
codesign -f -s - target/universal-apple-darwin/debug/libmirrord_layer.dylib

# build mirrord package
MIRRORD_MACOS_ARM64_LIBRARY=../../../target/aarch64-apple-darwin/debug/libmirrord_layer.dylib MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build -p mirrord --target=aarch64-apple-darwin
MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build -p mirrord --target=x86_64-apple-darwin

# create universal binary for mirrord and sign
lipo -create -output target/universal-apple-darwin/debug/mirrord target/aarch64-apple-darwin/debug/mirrord target/x86_64-apple-darwin/debug/mirrord
codesign -f -s - target/universal-apple-darwin/debug/mirrord

