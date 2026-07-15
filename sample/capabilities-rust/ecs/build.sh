#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage:
  ./build.sh <backend|frontend-next|sessions-manager> [local-tag] [--debug]

Examples:
  ./build.sh backend
  ./build.sh frontend-next
  ./build.sh sessions-manager
  ./build.sh backend --debug
  ./build.sh backend custom-backend-tag
  ./build.sh backend custom-backend-tag --debug
EOF
}

component=""
local_tag=""
debug=0

for arg in "$@"; do
  case "$arg" in
    --debug)
      debug=1
      ;;
    *)
      case "$component" in
        "") component="$arg" ;;
        *)
          case "$local_tag" in
            "") local_tag="$arg" ;;
            *)
              usage
              exit 1
              ;;
          esac
          ;;
      esac
      ;;
  esac
done

if [ -z "$component" ]; then
  usage
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mirrord_root=$(CDPATH= cd -- "$script_dir/../../.." && pwd)

case "$component" in
  backend)
    default_local_tag="capabilities-rust-backend"
    dockerfile_path="$mirrord_root/sample/capabilities-rust/backend/Dockerfile"
    deps_builder_dockerfile="$mirrord_root/sample/capabilities-rust/mirrord-dependancies-builder/Dockerfile"
    build_context="$mirrord_root/sample/capabilities-rust"
    deps_builder_context="$mirrord_root"
    ;;
  frontend-next)
    default_local_tag="capabilities-rust-frontend-next"
    dockerfile_path="$mirrord_root/sample/capabilities-rust/frontend-next/Dockerfile"
    build_context="$mirrord_root/sample/capabilities-rust/frontend-next"
    ;;
  sessions-manager)
    default_local_tag="sessions-manager:local"
    dockerfile_path="$mirrord_root/../operator/services/sessions-manager/Dockerfile"
    build_context="$mirrord_root/../operator"
    ;;
  *)
    printf 'Unknown component: %s\n' "$component" >&2
    usage
    exit 1
    ;;
esac

local_tag="${local_tag:-$default_local_tag}"

if [ "$debug" -eq 1 ]; then
  release="0"
else
  release="1"
fi

build_deps_builder() {
  printf 'Building mirrord-deps-builder image as mirrord-deps-builder:remote\n'
  docker build -t mirrord-deps-builder:remote \
    --build-arg RELEASE="$release" \
    --target remote \
    -f "$deps_builder_dockerfile" \
    "$deps_builder_context"
}

if [ "$component" = "backend" ]; then
  build_deps_builder
fi

printf 'Building %s image locally as %s\n' "$component" "$local_tag"

if [ "$component" = "backend" ]; then
  docker build -t "$local_tag" \
    --build-arg RELEASE="$release" \
    -f "$dockerfile_path" \
    --target mirrord-remote \
    "$build_context"
else
  docker build -t "$local_tag" \
    --build-arg RELEASE="$release" \
    -f "$dockerfile_path" \
    "$build_context"
fi
