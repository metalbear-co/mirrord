#!/usr/bin/env bash
set -euo pipefail

# Resolve directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Go library source that export C server APIs
SERVER_GO_FILE="$SCRIPT_DIR/server/main.go"
# Dynamic library (c-shared)
SERVER_C_SHARED_LIB="$SCRIPT_DIR/server/libgo_server_c_shared.so"
# Static library (c-archive)
SERVER_C_ARCHIVE_LIB="$SCRIPT_DIR/server/libgo_server_c_archive.a"

# CPP library that wraps exported server APIs from static CGO library 
SERVER_CPP_FILE="$SCRIPT_DIR/server/lib.cpp"
SERVER_CPP_WRAPPER_LIB="$SCRIPT_DIR/server/libcpp_server.so"


# Go library source that export C fileops APIs
FILEOPS_GO_FILE="$SCRIPT_DIR/fileops/main.go"
# Dynamic library (c-shared)
FILEOPS_C_SHARED_LIB="$SCRIPT_DIR/fileops/libgo_fileops_c_shared.so"
# Static library (c-archive)
FILEOPS_C_ARCHIVE_LIB="$SCRIPT_DIR/fileops/libgo_fileops_c_archive.a"

# CPP library that wraps exported fileops APIs from static CGO library 
FILEOPS_CPP_FILE="$SCRIPT_DIR/fileops/lib.cpp"
FILEOPS_CPP_WRAPPER_LIB="$SCRIPT_DIR/fileops/libcpp_fileops.so"

# CPP app that dlopen server and fileops dynamic CGO library
C_SHARED_CPP_FILE="$SCRIPT_DIR/main_c_shared.cpp"
C_SHARED_OUT_BIN="$SCRIPT_DIR/out.dlopen_cgo_c_shared"

# CPP app that dlopen the CPP wrapper libraries of static CGO libraries
C_ARCHIVE_CPP_FILE="$SCRIPT_DIR/main_c_archive.cpp"
C_ARCHIVE_OUT_BIN="$SCRIPT_DIR/out.dlopen_cpp_wrapper_cgo_c_archive"

echo "Script directory: $SCRIPT_DIR"

# Ensure files exist
if [[ ! -f "$SERVER_GO_FILE" ]]; then
    echo "ERROR: server/main.go not found in $SCRIPT_DIR"
    exit 1
fi

if [[ ! -f "$FILEOPS_GO_FILE" ]]; then
    echo "ERROR: fileops/main.go not found in $SCRIPT_DIR"
    exit 1
fi

if [[ ! -f "$C_SHARED_CPP_FILE" ]]; then
    echo "ERROR: main.cpp not found in $SCRIPT_DIR"
    exit 1
fi

go version

echo "Building Go c-shared server library..."
go build -buildmode=c-shared -o "$SERVER_C_SHARED_LIB" "$SERVER_GO_FILE"

echo "Building Go c-shared file ops library..."
go build -buildmode=c-shared -o "$FILEOPS_C_SHARED_LIB" "$FILEOPS_GO_FILE"

echo "Building C++ c-shared loader app..."
g++ "$C_SHARED_CPP_FILE" -o "$C_SHARED_OUT_BIN" -ldl

echo "Done! c-shared artifacts:"
echo " - $SERVER_C_SHARED_LIB"
echo " - $FILEOPS_C_SHARED_LIB"
echo " - $C_SHARED_OUT_BIN"

echo "Building Go c-archive server library..."
go build -buildmode=c-archive -o "$SERVER_C_ARCHIVE_LIB" "$SERVER_GO_FILE"

echo "Building C++ server dynamic wrapper library..."
g++ -fPIC -shared $SERVER_CPP_FILE $SERVER_C_ARCHIVE_LIB -o $SERVER_CPP_WRAPPER_LIB -lpthread -ldl

echo "Building Go c-archive file ops library..."
go build -buildmode=c-archive -o "$FILEOPS_C_ARCHIVE_LIB" "$FILEOPS_GO_FILE"

echo "Building C++ file ops dynamic wrapper library..."
g++ -fPIC -shared $FILEOPS_CPP_FILE $FILEOPS_C_ARCHIVE_LIB -o $FILEOPS_CPP_WRAPPER_LIB -lpthread -ldl

echo "Building C++ c-archive loaded app..."
g++ $C_ARCHIVE_CPP_FILE -o $C_ARCHIVE_OUT_BIN -ldl

echo "Done! c-archive artifacts:"
echo " - $SERVER_C_ARCHIVE_LIB"
echo " - $SERVER_CPP_WRAPPER_LIB"
echo " - $FILEOPS_C_ARCHIVE_LIB"
echo " - $FILEOPS_CPP_WRAPPER_LIB"
echo " - $C_ARCHIVE_OUT_BIN"
