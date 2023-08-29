#!/bin/sh
# This script builds C apps used during integration and e2e tests.
# It should be run from the repo root directory.
#
# Optional argument: name of compiled binaries. Default is "out.c_test_app".

set -e

if [ $# -lt 1 ]
then
  name="out.c_test_app"
else
  name=$1
fi

for source_file in $(find . -name "*.c" -not -path "./target/*" -not -path "*/venv/*" -not -path "*/node_modules/*")
do
    directory=$(dirname "$source_file")
    out_file="$directory/$name"
    echo "$out_file"
    gcc "$source_file" -o "$out_file"
done
