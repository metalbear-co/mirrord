#!/usr/bin/env bash
set -euo pipefail

# Resolve directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

GO_FILE="$SCRIPT_DIR/main.go"
CPP_FILE="$SCRIPT_DIR/main.cpp"
SO_FILE="$SCRIPT_DIR/libgo_server.so"
OUT_BIN="$SCRIPT_DIR/out.cpp_dlopen_cgo"

check_dynamic() {
    local artifact="$1"
    local ldd_output

    if ! ldd_output="$(ldd "$artifact" 2>&1)"; then
        echo "ERROR: failed to inspect dynamic linkage for $artifact"
        echo "$ldd_output"
        exit 1
    fi

    if grep -Eq 'not a dynamic executable|statically linked' <<<"$ldd_output"; then
        echo "ERROR: expected $artifact to be dynamically linked"
        echo "$ldd_output"
        exit 1
    fi
}

echo "Script directory: $SCRIPT_DIR"

# Ensure files exist
if [[ ! -f "$GO_FILE" ]]; then
    echo "ERROR: main.go not found in $SCRIPT_DIR"
    exit 1
fi

if [[ ! -f "$CPP_FILE" ]]; then
    echo "ERROR: main.cpp not found in $SCRIPT_DIR"
    exit 1
fi

echo "Building Go shared library..."
go build -buildmode=c-shared -o "$SO_FILE" "$GO_FILE"

echo "Building C++ loader app..."
g++ "$CPP_FILE" -o "$OUT_BIN" -ldl

echo "Verifying dynamic linkage..."
check_dynamic "$SO_FILE"
check_dynamic "$OUT_BIN"

echo "Done!"
echo "Artifacts:"
echo " - $SO_FILE"
echo " - $OUT_BIN"
