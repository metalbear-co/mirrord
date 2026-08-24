#!/usr/bin/env sh
set -eu

cat <<'EOF'

███╗   ███╗██╗██████╗ ██████╗ ██████╗ ██████╗     ██████╗ ██╗   ██╗██╗██╗     ██████╗ ███████╗██████╗
████╗ ████║██║██╔══██╗██╔══██╗██╔══██╗██╔══██╗    ██╔══██╗██║   ██║██║██║     ██╔══██╗██╔════╝██╔══██╗
██╔████╔██║██║██████╔╝██████╔╝██║  ██║██║  ██║    ██████╔╝██║   ██║██║██║     ██║  ██║█████╗  ██████╔╝
██║╚██╔╝██║██║██╔══██╗██╔══██╗██║  ██║██║  ██║    ██╔══██╗██║   ██║██║██║     ██║  ██║██╔══╝  ██╔══██╗
██║ ╚═╝ ██║██║██║  ██║██║  ██║██████╔╝██████╔╝    ██████╔╝╚██████╔╝██║███████╗██████╔╝███████╗██║  ██║
╚═╝     ╚═╝╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚═════╝     ╚═════╝  ╚═════╝ ╚═╝╚══════╝╚═════╝ ╚══════╝╚═╝  ╚═╝

                                  M I R R O R D   B U I L D E R

EOF

usage() {
  cat <<'EOF'
Usage:
  ./build.sh <backend|frontend-next|sessions-manager> [local-tag] [--debug]
  ./build.sh ... [--mirrord-root PATH] [--operator-root PATH]

Examples:
  ./build.sh backend
  ./build.sh frontend-next
  ./build.sh sessions-manager
  ./build.sh backend --debug
  ./build.sh backend custom-backend-tag
  ./build.sh backend custom-backend-tag --debug
  ./build.sh sessions-manager --mirrord-root ../mirrord-worktree --operator-root ../operator-worktree
EOF
}

component=""
local_tag=""
mirrord_root_arg=""
operator_root_arg=""
debug=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --debug)
      debug=1
      shift
      ;;
    --mirrord-root|--operator-root)
      option="$1"
      if [ "$#" -lt 2 ]; then
        printf '%s requires a path\n' "$option" >&2
        usage
        exit 1
      fi
      case "$option" in
        --mirrord-root) mirrord_root_arg="$2" ;;
        --operator-root) operator_root_arg="$2" ;;
      esac
      shift 2
      ;;
    --mirrord-root=*|--operator-root=*)
      option=${1%%=*}
      option_value=${1#*=}
      if [ -z "$option_value" ]; then
        printf '%s requires a path\n' "$option" >&2
        usage
        exit 1
      fi
      case "$option" in
        --mirrord-root) mirrord_root_arg="$option_value" ;;
        --operator-root) operator_root_arg="$option_value" ;;
      esac
      shift
      ;;
    *)
      case "$component" in
        "") component="$1" ;;
        *)
          case "$local_tag" in
            "") local_tag="$1" ;;
            *)
              usage
              exit 1
              ;;
          esac
          ;;
      esac
      shift
      ;;
  esac
done

if [ -z "$component" ]; then
  usage
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

resolve_paths() {
  if [ -n "$mirrord_root_arg" ]; then
    mirrord_root=$(CDPATH= cd -- "$mirrord_root_arg" && pwd)
  else
    mirrord_root=$(CDPATH= cd -- "$script_dir/../../../.." && pwd)
  fi

  if [ -n "$operator_root_arg" ]; then
    operator_root=$(CDPATH= cd -- "$operator_root_arg" && pwd)
  else
    operator_root=$(CDPATH= cd -- "$mirrord_root/../operator" && pwd)
  fi
}

resolve_paths
printf 'Using mirrord folder: %s\n' "$mirrord_root"
printf 'Using operator folder: %s\n' "$operator_root"

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
    dockerfile_path="$operator_root/services/sessions-manager/Dockerfile"
    build_context="$operator_root"
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
