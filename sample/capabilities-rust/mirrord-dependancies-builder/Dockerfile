## syntax=docker/dockerfile:1.7
FROM ghcr.io/metalbear-co/ci-agent-build:latest AS builder
WORKDIR /repo

ARG TARGETARCH
ARG BUILDARCH
ARG RELEASE=1

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY xtask ./xtask
COPY mirrord ./mirrord
COPY sample ./sample
COPY test-utils ./test-utils
COPY tests ./tests
COPY medschool ./medschool
COPY packages ./packages

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV SCCACHE_VERSION=0.14.0
RUN if [ "${BUILDARCH:-amd64}" = "arm64" ]; then SCCACHE_ARCH=aarch64; else SCCACHE_ARCH=x86_64; fi && \
    curl -fsSL https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl.tar.gz \
    | tar -xzv --strip-components=1 -C /usr/local/bin sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl/sccache && \
    chmod +x /usr/local/bin/sccache
ENV RUSTC_WRAPPER=/usr/local/bin/sccache

RUN --mount=type=cache,target=/root/.cargo/registry,id=mirrord-deps-builder-cargo-registry-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,id=mirrord-deps-builder-cargo-git-$TARGETARCH,sharing=locked \
    cargo fetch

RUN mkdir -p /repo/packages/monitor/dist

FROM builder AS local

RUN --mount=type=cache,target=/root/.cargo/registry,id=mirrord-deps-builder-cargo-registry-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,id=mirrord-deps-builder-cargo-git-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cache,id=mirrord-deps-builder-sccache-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/repo/target,id=mirrord-deps-builder-target-$TARGETARCH,sharing=locked \
    set -eu; \
    if [ "${TARGETARCH:-amd64}" = "arm64" ]; then \
      PLATFORM=linux-aarch64; \
      TARGET_TRIPLE=aarch64-unknown-linux-gnu; \
    else \
      PLATFORM=linux-x86_64; \
      TARGET_TRIPLE=x86_64-unknown-linux-gnu; \
    fi; \
    if [ "$RELEASE" = "1" ]; then \
      release_flag=--release; \
      target_dir=release; \
    else \
      release_flag=; \
      target_dir=debug; \
    fi; \
    cargo xtask build-cli --platform "$PLATFORM" --no-ui ${release_flag:-} --locked && \
    mkdir -p /opt/mirrord/bin && \
    cp "/repo/target/${TARGET_TRIPLE}/${target_dir}/mirrord" /opt/mirrord/bin/mirrord

FROM builder AS remote

RUN --mount=type=cache,target=/root/.cargo/registry,id=mirrord-deps-builder-cargo-registry-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,id=mirrord-deps-builder-cargo-git-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cache,id=mirrord-deps-builder-sccache-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/repo/target,id=mirrord-deps-builder-target-$TARGETARCH,sharing=locked \
    set -eu; \
    if [ "${TARGETARCH:-amd64}" = "arm64" ]; then \
      PLATFORM=linux-aarch64; \
      TARGET_TRIPLE=aarch64-unknown-linux-gnu; \
    else \
      PLATFORM=linux-x86_64; \
      TARGET_TRIPLE=x86_64-unknown-linux-gnu; \
    fi; \
    if [ "$RELEASE" = "1" ]; then \
      release_flag=--release; \
      target_dir=release; \
    else \
      release_flag=; \
      target_dir=debug; \
    fi; \
    cargo xtask build-remote-bootstrap --platform "$PLATFORM" ${release_flag:-} --locked && \
    mkdir -p /opt/mirrord/lib && \
    cp "/repo/target/${TARGET_TRIPLE}/${target_dir}/libmirrord_remote_bootstrap.so" /opt/mirrord/lib/libmirrord_remote_bootstrap.so
