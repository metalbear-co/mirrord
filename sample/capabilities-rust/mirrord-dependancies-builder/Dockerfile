## syntax=docker/dockerfile:1.7
FROM ghcr.io/metalbear-co/ci-agent-build:latest AS builder
WORKDIR /repo

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

#RUN --mount=type=cache,target=/root/.cargo/registry \
#    --mount=type=cache,target=/root/.cargo/git \
#    --mount=type=cache,target=/repo/target \
#    cargo xtask build-layer --release --locked

#RUN --mount=type=cache,target=/root/.cargo/registry \
#    --mount=type=cache,target=/root/.cargo/git \
#    --mount=type=cache,target=/repo/target \
#    cargo xtask build-cli --no-wizard --no-monitor --release --locked

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/repo/target \
    cargo xtask build-serverless-bootstrap --release --locked && \
    mkdir -p /out/bin /out/lib && \
    cp /repo/target/release/mirrord-agent /out/bin/mirrord-agent && \
    cp /repo/target/release/libmirrord_serverless_bootstrap.so /out/lib/libmirrord_serverless_bootstrap.so

#    cp /repo/target/x86_64-unknown-linux-gnu/release/mirrord /out/bin/mirrord && \
#    cp /repo/target/x86_64-unknown-linux-gnu/release/libmirrord_layer.so /out/lib/libmirrord_layer.so && \

FROM debian:bookworm-slim
RUN mkdir -p /opt/mirrord/bin /opt/mirrord/lib

#COPY --from=builder /out/bin/mirrord /opt/mirrord/bin/mirrord
COPY --from=builder /out/bin/mirrord-agent /opt/mirrord/bin/mirrord-agent
#COPY --from=builder /out/lib/libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so
COPY --from=builder /out/lib/libmirrord_serverless_bootstrap.so /opt/mirrord/lib/libmirrord_serverless_bootstrap.so
