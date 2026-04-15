variable "REGISTRY" {
  default = "ghcr.io/metalbear-co"
}

# Commit SHA, used to tag CI images alongside :latest.
variable "SHA" {
  default = ""
}

# Comma-separated tags for the agent image.
# Override in CI: AGENT_TAGS="ghcr.io/metalbear-co/mirrord:1.2.3,ghcr.io/metalbear-co/mirrord:latest"
variable "AGENT_TAGS" {
  default = "${REGISTRY}/mirrord:dev"
}

# Comma-separated tags for the CLI image.
variable "CLI_TAGS" {
  default = "${REGISTRY}/mirrord-cli:dev"
}

# Platforms for product images. Override to target a single platform (e.g. linux/amd64 for local test builds).
variable "PLATFORMS" {
  default = "linux/amd64,linux/arm64"
}

# Shared buildx cache settings for product images. Override to empty strings to disable
# importing from and exporting to the GitHub Actions cache backend.
variable "PRODUCT_CACHE_FROM" {
  default = "type=gha"
}

variable "PRODUCT_CACHE_TO" {
  default = "type=gha,mode=max"
}

# Product images built from this repo.
group "default" {
  targets = ["agent", "cli"]
}

# CI base images used by build and test jobs.
group "ci-images" {
  targets = [
    "ci-agent-builder",
    "ci-agent-runtime",
    "ci-layer-build-x86_64",
    "ci-layer-build-aarch64",
    "ci-rust-build",
  ]
}

target "agent" {
  context    = "."
  dockerfile = "mirrord/agent/Dockerfile"
  platforms  = split(",", PLATFORMS)
  tags       = split(",", AGENT_TAGS)
  cache-from = compact([PRODUCT_CACHE_FROM])
  cache-to   = compact([PRODUCT_CACHE_TO])
  contexts = {
    "ghcr.io/metalbear-co/ci-agent-build:fc6a43e83803b870361cb2ad801d7f0e23d2dd21"  = "target:ci-agent-builder"
    "ghcr.io/metalbear-co/ci-agent-runtime:30dca9bcb32306a028178cac371b1e47e403916c" = "target:ci-agent-runtime"
  }
}

target "cli" {
  context    = "."
  dockerfile = "mirrord/cli/Dockerfile"
  platforms  = split(",", PLATFORMS)
  tags       = split(",", CLI_TAGS)
  cache-from = compact([PRODUCT_CACHE_FROM])
  cache-to   = compact([PRODUCT_CACHE_TO])
  contexts = {
    "ghcr.io/metalbear-co/ci-agent-build:fc6a43e83803b870361cb2ad801d7f0e23d2dd21" = "target:ci-agent-builder"
  }
}

# Builder image: Rust toolchain + cross-compilation tools for building the agent.
target "ci-agent-builder" {
  context    = "./ci"
  dockerfile = "agent-build/builder.Dockerfile"
  platforms  = ["linux/amd64", "linux/arm64"]
  tags = compact([
    "${REGISTRY}/ci-agent-build:latest",
    SHA != "" ? "${REGISTRY}/ci-agent-build:${SHA}" : "",
  ])
}

# Runtime image: minimal scratch-based image with networking tools for the agent container.
target "ci-agent-runtime" {
  context    = "./ci"
  dockerfile = "agent-build/runtime.Dockerfile"
  platforms  = ["linux/amd64", "linux/arm64"]
  tags = compact([
    "${REGISTRY}/ci-agent-runtime:latest",
    SHA != "" ? "${REGISTRY}/ci-agent-runtime:${SHA}" : "",
  ])
}

# Cross-compilation image for mirrord-layer targeting x86_64 with old glibc (CentOS 7 / Amazon Linux 2).
target "ci-layer-build-x86_64" {
  context    = "./ci"
  dockerfile = "layer-build/x86_64.Dockerfile"
  platforms  = ["linux/amd64"]
  tags = compact([
    "${REGISTRY}/ci-layer-build:latest",
    SHA != "" ? "${REGISTRY}/ci-layer-build:${SHA}" : "",
  ])
}

# Cross-compilation image for mirrord-layer targeting aarch64 with old glibc (CentOS 7 / Amazon Linux 2).
target "ci-layer-build-aarch64" {
  context    = "./ci"
  dockerfile = "layer-build/aarch64.Dockerfile"
  platforms  = ["linux/amd64"]
  tags = compact([
    "${REGISTRY}/ci-layer-build-aarch64:latest",
    SHA != "" ? "${REGISTRY}/ci-layer-build-aarch64:${SHA}" : "",
  ])
}

# Base Rust build image with nightly toolchain and protobuf compiler.
target "ci-rust-build" {
  context    = "./ci"
  dockerfile = "rust-build/Dockerfile"
  platforms  = ["linux/amd64"]
  tags = compact([
    "${REGISTRY}/ci-rust-build:latest",
    SHA != "" ? "${REGISTRY}/ci-rust-build:${SHA}" : "",
  ])
}
