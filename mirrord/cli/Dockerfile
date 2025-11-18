FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:75752d06e0de748b7bc5e5b1a54575e3c968f5cf AS chef
ARG TARGETARCH
WORKDIR /build

COPY ./mirrord/agent/platform.sh /build/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# cargo-chef 0.1.69 breaks the build
RUN cargo install cargo-chef@0.1.72

FROM chef AS planner

COPY . /build/
RUN cargo +nightly-2025-08-01 chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /build/recipe.json recipe.json
RUN cargo +nightly-2025-08-01 chef cook --release --zigbuild --target $(cat /.platform) --recipe-path recipe.json

COPY . /build/
RUN cargo +nightly-2025-08-01 zigbuild -p mirrord -p mirrord-layer -Z bindeps --target $(cat /.platform) --release --locked
RUN cp /build/target/$(cat /.platform)/release/mirrord /mirrord
RUN cp /build/target/$(cat /.platform)/release/libmirrord_layer.so /libmirrord_layer.so

# For ARM64 builds, also build x86_64 version to support cross-platform containers
RUN if [ "$TARGETARCH" = "arm64" ]; then \
    cargo +nightly-2025-08-01 zigbuild -p mirrord-layer -Z bindeps --target x86_64-unknown-linux-gnu --release --locked && \
    cp /build/target/x86_64-unknown-linux-gnu/release/libmirrord_layer.so /libmirrord_layer_x86_64.so; \
    fi

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
