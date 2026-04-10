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

1>&2 echo "Using $(go version)"

for go_mod in $(find . -name "go\.mod")
do
    directory=$(dirname $go_mod)

    if [ "$(go env GOARCH)" = "arm64" ] && grep -q "rogchap.com/v8go" "$go_mod"
    then
        1>&2 echo "Skipping $directory on arm64 due to v8go x86_64-only dependency"
        continue
    fi

    cd $directory

    1>&2 echo "Building test app $directory/$1.go_test_app"
    go build -o "$1.go_test_app"

    cd - 1>/dev/null
done

1>&2 echo "All done"
