FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:fc6a43e83803b870361cb2ad801d7f0e23d2dd21 AS builder
ARG TARGETARCH
ARG BUILDARCH
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends nodejs npm && rm -rf /var/lib/apt/lists/*

COPY ./mirrord/agent/platform.sh /build/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
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

COPY . /build/
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-cli-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-cli-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/sccache,id=sccache-cli-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/build/target,id=target-cli-$TARGETARCH,sharing=locked \
    if sccache --start-server; then \
    echo "sccache server started"; \
    else \
    echo "sccache failed to start, continuing without cache"; \
    unset RUSTC_WRAPPER; \
    fi && \
    cargo +nightly-2026-02-24 zigbuild -p mirrord -Z bindeps --target $(cat /.platform) --release --features wizard && \
    if [ "$TARGETARCH" = "arm64" ]; then \
    cargo +nightly-2026-02-24 zigbuild -p mirrord-layer -Z bindeps --target x86_64-unknown-linux-gnu --release --locked && \
    cp /build/target/x86_64-unknown-linux-gnu/release/libmirrord_layer.so /libmirrord_layer_x86_64.so; \
    fi && \
    echo "sccache stats" && \
    (sccache --show-stats || echo "sccache stats unavailable") && \
    cp /build/target/$(cat /.platform)/release/mirrord /mirrord && \
    cp /build/target/$(cat /.platform)/release/libmirrord_layer.so /libmirrord_layer.so

FROM debian AS runtime
ARG TARGETARCH

RUN mkdir -p /opt/mirrord/bin && mkdir -p /opt/mirrord/lib && mkdir -p /opt/mirrord/tls
COPY --from=builder /mirrord /opt/mirrord/bin/mirrord
COPY --from=builder /libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so

# Create architecture-specific directories and copy libraries
RUN if [ "$TARGETARCH" = "arm64" ]; then \
    mkdir -p /opt/mirrord/lib/aarch64 /opt/mirrord/lib/x86_64 && \
    cp /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/lib/aarch64/libmirrord_layer.so; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
    mkdir -p /opt/mirrord/lib/x86_64 && \
    cp /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/lib/x86_64/libmirrord_layer.so; \
    fi

# Copy cross-architecture library for ARM64 builds
COPY --from=builder /libmirrord_layer_x86_64.so* /tmp/
RUN if [ "$TARGETARCH" = "arm64" ] && [ -f /tmp/libmirrord_layer_x86_64.so ]; then \
    cp /tmp/libmirrord_layer_x86_64.so /opt/mirrord/lib/x86_64/libmirrord_layer.so; \
    fi

VOLUME /opt/mirrord

RUN ln -s /opt/mirrord/bin/mirrord /usr/bin/mirrord
