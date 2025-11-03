#!/bin/sh
# This script builds a fat binary layer dylib for Mac OS X.
# Output will be in target/universal-apple-darwin/debug/shim.dylib
# Any arguments provided to this script are passed to `cargo build`.

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

