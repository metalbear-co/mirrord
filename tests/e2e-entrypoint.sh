#!/bin/bash
# Stages the test apps baked into this image into the mirrord checkout mounted at /workspace/mirrord,
# then runs the given command.
#
# `scripts/prepare_e2e.sh` overwrites them, so a checkout that has moved on rebuilds rather than
# testing stale code.

set -euo pipefail

root=/workspace/mirrord

if [ ! -d "$root" ]; then
    1>&2 echo ">>> No mirrord checkout at ${root}, skipping the prebuilt test apps"
elif [ -d /opt/e2e-artifacts ]; then
    # Files are given the checkout's owner, since a bind-mounted checkout belongs to the host user
    # and root-owned build output in it cannot be cleaned up without root.
    #
    # Extraction uses -P because the staged node_modules is full of pnpm symlinks pointing at
    # `../<package>@<version>/...`. Without it tar treats every link whose target contains `..`
    # as suspicious, writes a mode 000 placeholder file first and links it at the end; on the
    # macOS bind mount Docker Desktop gives a container, that placeholder cannot be reopened,
    # so every link fails and the placeholders are left behind in the checkout. The archive is
    # built into this image, so there is nothing to guard against.
    tar -cf - --numeric-owner \
        --owner="$(stat -c '%u' "$root")" \
        --group="$(stat -c '%g' "$root")" \
        -C /opt/e2e-artifacts . \
        | tar -xPf - -C "$root"

    1>&2 echo ">>> Staged prebuilt test apps into ${root}"
fi

exec "$@"
