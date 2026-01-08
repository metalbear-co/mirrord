#!/usr/bin/env bash
set -euo pipefail

# Resolve directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

SERVER_GO_FILE="$SCRIPT_DIR/server/main.go"
SERVER_SO_FILE="$SCRIPT_DIR/server/libgo_server.so"

FILEOPS_GO_FILE="$SCRIPT_DIR/fileops/main.go"
FILEOPS_SO_FILE="$SCRIPT_DIR/fileops/libgo_fileops.so"

CPP_FILE="$SCRIPT_DIR/main.cpp"
OUT_BIN="$SCRIPT_DIR/out.cpp_dlopen_cgo"

echo "Script directory: $SCRIPT_DIR"

# Ensure files exist
if [[ ! -f "$SERVER_GO_FILE" ]]; then
    echo "ERROR: main.go not found in $SCRIPT_DIR"
    exit 1
fi

if [[ ! -f "$FILEOPS_GO_FILE" ]]; then
    echo "ERROR: main.go not found in $SCRIPT_DIR"
    exit 1
fi

if [[ ! -f "$CPP_FILE" ]]; then
    echo "ERROR: main.cpp not found in $SCRIPT_DIR"
    exit 1
fi

go version
echo "Building Go shared server library..."
go build -buildmode=c-shared -o "$SERVER_SO_FILE" "$SERVER_GO_FILE"

echo "Building Go shared file ops library..."
go build -buildmode=c-shared -o "$FILEOPS_SO_FILE" "$FILEOPS_GO_FILE"

echo "Building C++ loader app..."
g++ "$CPP_FILE" -o "$OUT_BIN" -ldl

echo "Done!"
echo "Artifacts:"
echo " - $SERVER_SO_FILE"
echo " - $FILEOPS_SO_FILE"
echo " - $OUT_BIN"
