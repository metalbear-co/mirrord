# See here for image contents: https://github.com/devcontainers/images/blob/main/src/rust/.devcontainer/Dockerfile

# [Choice] Debian OS version (use bookworm, or bullseye on local arm64/Apple Silicon): bookworm, buster, bullseye
ARG VARIANT="buster"
FROM mcr.microsoft.com/devcontainers/rust:1-${VARIANT}

RUN apt-get update && apt-get install -y clang
RUN curl -Lo ./kind https://kind.sigs.k8s.io/dl/v0.16.0/kind-linux-amd64 && chmod +x ./kind && mv ./kind /usr/local/bin/kind
