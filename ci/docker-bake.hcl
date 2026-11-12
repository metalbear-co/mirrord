variable "REGISTRY" {
  default = "ghcr.io/metalbear-co"
}

# Commit SHA, used to tag CI images alongside :latest.
variable "SHA" {
  default = ""
}

# Platforms for the images. Override to target a single platform (e.g. linux/amd64 for local test builds).
variable "PLATFORMS" {
  default = "linux/amd64,linux/arm64"
}

# Shared buildx cache settings for product images. Empty by default; each caller opts in with a
# backend it can reach. Not `type=gha`: `mode=max` there crowds the repository out of its 10 GB
# Actions cache quota and evicts the Rust cache records.
variable "PRODUCT_CACHE_FROM" {
  default = ""
}

variable "PRODUCT_CACHE_TO" {
  default = ""
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

# Builder image: Rust toolchain + cross-compilation tools for building the agent.
target "ci-agent-builder" {
  context    = "."
  dockerfile = "ci/agent-build/builder.Dockerfile"
  platforms  = split(",", PLATFORMS)
  tags = compact([
    "${REGISTRY}/ci-agent-build:latest",
    SHA != "" ? "${REGISTRY}/ci-agent-build:${SHA}" : "",
  ])
}

# Runtime image: minimal scratch-based image with networking tools for the agent container.
target "ci-agent-runtime" {
  context    = "./ci"
  dockerfile = "agent-build/runtime.Dockerfile"
  platforms  = split(",", PLATFORMS)
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

# Base Rust build image with the pinned Rust toolchain and protobuf compiler.
target "ci-rust-build" {
  context    = "."
  dockerfile = "ci/rust-build/Dockerfile"
  platforms  = ["linux/amd64"]
  tags = compact([
    "${REGISTRY}/ci-rust-build:latest",
    SHA != "" ? "${REGISTRY}/ci-rust-build:${SHA}" : "",
  ])
}
