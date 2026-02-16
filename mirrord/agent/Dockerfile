FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:75752d06e0de748b7bc5e5b1a54575e3c968f5cf AS base
ARG TARGETARCH
WORKDIR /app

COPY ./mirrord/agent/platform.sh /app/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Install mold linker for faster linking
RUN apt-get update && apt-get install -y mold && rm -rf /var/lib/apt/lists/*

ENV SCCACHE_VERSION=0.14.0
RUN SCCACHE_ARCH=$(case ${BUILDARCH} in \
    amd64) echo "x86_64" ;; \
    arm64) echo "aarch64" ;; \
    *) echo "x86_64" ;; \
    esac) && \
    curl -fsSL https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl.tar.gz \
    | tar -xzv --strip-components=1 -C /usr/local/bin sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl/sccache && \
    chmod +x /usr/local/bin/sccache

# sccache configuration - will be used if GCS credentials are provided
ENV RUSTC_WRAPPER=/usr/local/bin/sccache

# Use mold linker for faster linking
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"



# Copy order is important here, since we want the cache to be invalidated as less as possible
# so we start with the most static files, then the most dynamic ones
COPY .cargo /app/.cargo
COPY mirrord/macros /app/mirrord/macros
COPY mirrord/tls-util /app/mirrord/tls-util
COPY mirrord/protocol /app/mirrord/protocol
COPY mirrord/agent /app/mirrord/agent
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/

# workspace files requires all members to exist while most of those aren't really needed
# so we remove from the workspace
RUN sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n]' /app/Cargo.toml


# Build final binary with sccache and BuildKit cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/root/.cache,id=sccache-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/app/target,id=target-agent-$TARGETARCH,sharing=locked \
    if sccache --start-server; then \
    echo "sccache server started"; \
    else \
    echo "sccache failed to start, continuing without cache"; \
    unset RUSTC_WRAPPER; \
    fi && \
    cargo +nightly-2025-08-01 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release && \
    echo "sccache stats" && \
    (sccache --show-stats || echo "sccache stats unavailable") && \
    # Copy binary out of cached target directory
    cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:30dca9bcb32306a028178cac371b1e47e403916c
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]
