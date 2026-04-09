# Using the agent's build image, that has rustup targets, zigbuild etc.
FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:fc6a43e83803b870361cb2ad801d7f0e23d2dd21 AS build-env
ARG TARGETARCH
ARG BUILDARCH

WORKDIR /build
COPY . .

# Translate docker's platform to cargo's target.
RUN /build/platform.sh


# Build binary.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV SCCACHE_VERSION=0.14.0
ENV SCCACHE_DIR=/sccache

RUN SCCACHE_ARCH=$(case ${BUILDARCH} in \
    amd64) echo "x86_64" ;; \
    arm64) echo "aarch64" ;; \
    *) echo "x86_64" ;; \
    esac) && \
    curl -fsSL https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl.tar.gz \
    | tar -xzv --strip-components=1 -C /usr/local/bin sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl/sccache && \
    chmod +x /usr/local/bin/sccache

ENV RUSTC_WRAPPER=/usr/local/bin/sccache

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-rust-sqs-printer-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-rust-sqs-printer-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/sccache,id=sccache-rust-sqs-printer-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/build/target,id=target-rust-sqs-printer-$TARGETARCH,sharing=locked \
    if sccache --start-server; then \
    echo "sccache server started"; \
    else \
    echo "sccache failed to start, continuing without cache"; \
    unset RUSTC_WRAPPER; \
    fi && \
    cargo zigbuild -Z bindeps --target $(cat /.platform) && \
    echo "sccache stats" && \
    (sccache --show-stats || echo "sccache stats unavailable") && \
    cp /build/target/$(cat /.platform)/debug/rust-sqs-printer /rust-sqs-printer

# Slim final layer with only the binary.
FROM debian:stable-slim
# TODO: optimize this?
RUN apt-get update
RUN apt-get install -y --no-install-recommends ca-certificates
RUN update-ca-certificates

WORKDIR /app
COPY --from=build-env /rust-sqs-printer .

CMD ["./rust-sqs-printer"]
