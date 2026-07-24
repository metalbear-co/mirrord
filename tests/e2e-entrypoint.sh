#!/bin/bash
# Stages the test apps baked into this image into the mirrord checkout mounted at /workspace/mirrord,
# then runs the given command.
#
# Both suites locate their apps inside that checkout, so the artifacts have to land there rather
# than stay at some path of their own.
#
# Staged apps are whatever the image was built from. `scripts/prepare_e2e.sh` overwrites them, so a
# checkout that has moved on rebuilds them rather than testing stale code.

set -euo pipefail

root=/workspace/mirrord

if [ ! -d "$root" ]; then
    1>&2 echo ">>> No mirrord checkout at ${root}, skipping the prebuilt test apps"
elif [ -d /opt/e2e-artifacts ]; then
    # Files are given the checkout's owner, since a bind-mounted checkout belongs to the host user
    # and root-owned build output in it cannot be cleaned up without root.
    tar -cf - --numeric-owner \
        --owner="$(stat -c '%u' "$root")" \
        --group="$(stat -c '%g' "$root")" \
        -C /opt/e2e-artifacts . \
        | tar -xf - -C "$root"

    1>&2 echo ">>> Staged prebuilt test apps into ${root}"
fi

exec "$@"
