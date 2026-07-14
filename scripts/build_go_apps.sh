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

app_name=$1

# Let Go download the toolchain a module's `go` directive asks for, instead of
# failing under the GOTOOLCHAIN=local that newer setup-go versions export.
export GOTOOLCHAIN=auto

1>&2 echo "Using $(go version)"

for go_mod in $(find . -name "go\.mod")
do
    directory=$(dirname $go_mod)
    cd $directory

    package_info=$(go list -e -f '{{len .GoFiles}} {{len .CgoFiles}} {{len .IgnoredGoFiles}}' .)
    set -- $package_info
    go_files=$1
    cgo_files=$2
    ignored_go_files=$3

    if [ "$go_files" -eq 0 ] && [ "$cgo_files" -eq 0 ] && [ "$ignored_go_files" -gt 0 ]
    then
        1>&2 echo "Skipping test app $directory/$app_name.go_test_app: no buildable Go files for $(go env GOOS)/$(go env GOARCH)"
        cd - 1>/dev/null
        continue
    fi

    1>&2 echo "Building test app $directory/$app_name.go_test_app"
    go build -o "$app_name.go_test_app"
    cd - 1>/dev/null
done

1>&2 echo "All done"
