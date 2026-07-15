## syntax=docker/dockerfile:1.7
FROM ghcr.io/metalbear-co/ci-agent-build:latest AS builder
WORKDIR /repo

ARG TARGETARCH

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY xtask ./xtask
COPY mirrord ./mirrord
COPY sample ./sample
COPY test-utils ./test-utils
COPY tests ./tests
COPY medschool ./medschool
COPY packages ./packages

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch --locked

RUN mkdir -p /repo/packages/monitor/dist

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/repo/target \
    set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) PLATFORM=linux-x86_64 ; TARGET_TRIPLE=x86_64-unknown-linux-gnu ;; \
      arm64) PLATFORM=linux-aarch64 ; TARGET_TRIPLE=aarch64-unknown-linux-gnu ;; \
      *) echo "Unsupported TARGETARCH: ${TARGETARCH:-unset}" >&2; exit 1 ;; \
    esac; \
    cargo xtask build-layer --platform "$PLATFORM" --locked && \
    cargo xtask build-cli --platform "$PLATFORM" --no-ui --locked && \
    cargo xtask build-remote-bootstrap --platform "$PLATFORM" --locked && \
    mkdir -p /out/bin /out/lib && \
    cp "/repo/target/${TARGET_TRIPLE}/debug/mirrord" /out/bin/mirrord && \
    cp "/repo/target/${TARGET_TRIPLE}/debug/libmirrord_layer.so" /out/lib/libmirrord_layer.so && \
    cp "/repo/target/${TARGET_TRIPLE}/debug/mirrord-agent" /out/bin/mirrord-agent && \
    cp "/repo/target/${TARGET_TRIPLE}/debug/libmirrord_remote_bootstrap.so" /out/lib/libmirrord_remote_bootstrap.so

FROM debian:bookworm-slim
RUN mkdir -p /opt/mirrord/bin /opt/mirrord/lib

COPY --from=builder /out/bin/mirrord /opt/mirrord/bin/mirrord
COPY --from=builder /out/bin/mirrord-agent /opt/mirrord/bin/mirrord-agent
COPY --from=builder /out/lib/libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so
COPY --from=builder /out/lib/libmirrord_remote_bootstrap.so /opt/mirrord/lib/libmirrord_remote_bootstrap.so
