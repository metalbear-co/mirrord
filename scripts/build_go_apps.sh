#!/bin/sh
# This script builds Go apps used during integration and e2e tests.
# It builds every Go module found in the repo
# and should be run from the repo root directory.

set -e

if [ $# -ne 1 ]
then
    1>&2 echo "USAGE: $0 <name>\n\n\tAll compiled apps will be named <name>.go_test_app\n\tThis script should be run from the root of the project"
    exit 1
fi

requested_minor="$1"
requested_version="1.${requested_minor}"

1>&2 echo "Using $(go version)"

for go_mod in $(find . -name "go\.mod")
do
    directory=$(dirname "$go_mod")
    cd "$directory"

    required_version=$(awk '/^go[ \t]+/ {print $2; exit}' go.mod | tr -d ' ')
    if [ -z "$required_version" ]; then
        required_version="0.0"
    fi

    python3 - "$requested_version" "$required_version" <<'PY2'
import sys

def parse(ver: str):
    return tuple(int(part) for part in ver.split('.'))

requested = parse(sys.argv[1])
required = parse(sys.argv[2])

if requested < required:
    sys.exit(1)
PY2

    if [ $? -ne 0 ]; then
        1>&2 echo "Skipping $directory because it requires Go $required_version"
        cd - 1>/dev/null
        continue
    fi

    1>&2 echo "Building test app $directory/$1.go_test_app"
    go build -o "$1.go_test_app"

    cd - 1>/dev/null
done

1>&2 echo "All done"
