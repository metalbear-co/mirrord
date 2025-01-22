FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:602d397f8e2088ca6ef229e7da0f86490153d8b7 AS chef
ARG TARGETARCH
WORKDIR /build

COPY ./mirrord/agent/platform.sh /build/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# cargo-chef 0.1.69 breaks the build
RUN cargo install cargo-chef@0.1.68

FROM chef AS planner

COPY . /build/
RUN cargo +nightly-2025-01-22 chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /build/recipe.json recipe.json
RUN cargo +nightly-2025-01-22 chef cook --release --zigbuild --target $(cat /.platform) --recipe-path recipe.json

COPY . /build/
RUN cargo +nightly-2025-01-22 zigbuild -p mirrord -p mirrord-layer -Z bindeps --target $(cat /.platform) --release --locked
RUN cp /build/target/$(cat /.platform)/release/mirrord /mirrord
RUN cp /build/target/$(cat /.platform)/release/libmirrord_layer.so /libmirrord_layer.so

FROM debian AS runtime

RUN mkdir -p /opt/mirrord/bin && mkdir -p /opt/mirrord/lib && mkdir -p /opt/mirrord/tls
COPY --from=builder /mirrord /opt/mirrord/bin/mirrord
COPY --from=builder /libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so

VOLUME /opt/mirrord

RUN ln -s /opt/mirrord/bin/mirrord /usr/bin/mirrord
