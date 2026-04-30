#!/bin/sh

set -eu

placeholder="${PWD}/scripts/cargo-shear.sh"

export MIRRORD_LAYER_FILE="$placeholder"
export MIRRORD_LAYER_FILE_MACOS_ARM64="$placeholder"

exec cargo shear --expand "$@"
