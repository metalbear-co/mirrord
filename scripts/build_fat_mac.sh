#!/bin/sh
# This script builds a fat binary for Mac OS X.
# Output will be in target/universal-apple-darwin/debug/mirrord
# If compilation fails, try running:
# rustup target add --toolchain nightly x86_64-apple-darwin

set -e
cargo build -p mirrord-layer --target=x86_64-apple-darwin
cargo build -p mirrord-layer --target=aarch64-apple-darwin
cp target/aarch64-apple-darwin/debug/libmirrord_layer.dylib target/aarch64-apple-darwin/debug/libmirrord_layer_arm64e.dylib
printf '\x02' | dd of=target/aarch64-apple-darwin/debug/libmirrord_layer_arm64e.dylib bs=1 seek=8 count=1 conv=notrunc
codesign -f -s - target/aarch64-apple-darwin/debug/libmirrord_layer_arm64e.dylib
codesign -f -s - target/x86_64-apple-darwin/debug/libmirrord_layer.dylib
mkdir -p target/universal-apple-darwin/debug
lipo -create -output target/universal-apple-darwin/debug/libmirrord_layer.dylib target/aarch64-apple-darwin/debug/libmirrord_layer.dylib target/x86_64-apple-darwin/debug/libmirrord_layer.dylib target/aarch64-apple-darwin/debug/libmirrord_layer_arm64e.dylib
codesign -f -s - target/universal-apple-darwin/debug/libmirrord_layer.dylib
MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build -p mirrord --target=aarch64-apple-darwin
MIRRORD_LAYER_FILE=../../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo build -p mirrord --target=x86_64-apple-darwin
lipo -create -output target/universal-apple-darwin/debug/mirrord target/aarch64-apple-darwin/debug/mirrord target/x86_64-apple-darwin/debug/mirrord
codesign -f -s - target/universal-apple-darwin/debug/mirrord

