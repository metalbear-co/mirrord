FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.toml
COPY backend/Cargo.toml backend/Cargo.toml
COPY backend/src backend/src
RUN cargo build -p capabilities-rust-backend

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app

RUN --mount=type=bind,from=builder,source=/app/target,target=/builder-target,ro \
    cp "/builder-target/debug/capabilities-rust-backend" /app/capabilities-rust-backend

ENTRYPOINT ["/app/capabilities-rust-backend"]

FROM runtime AS mirrord-remote

RUN mkdir -p /opt/mirrord/lib
COPY --from=mirrord-deps-builder:remote /opt/mirrord/lib/libmirrord_remote_bootstrap.so /opt/mirrord/lib/libmirrord_remote_bootstrap.so

# ENV MIRRORD_REMOTE_APIKEY # attached in runtime
ENV MIRRORD_REMOTE_SERVICE=demo-service \
    LD_PRELOAD=/opt/mirrord/lib/libmirrord_remote_bootstrap.so

ENTRYPOINT ["/app/capabilities-rust-backend"]

FROM runtime AS mirrord-local

COPY ecs/demo.env .env
COPY ecs/mirrord.json mirrord.json

RUN mkdir -p /opt/mirrord/bin
COPY --from=mirrord-deps-builder:local /opt/mirrord/bin/mirrord /opt/mirrord/bin/mirrord

ENTRYPOINT ["/opt/mirrord/bin/mirrord", "exec", "-f", "mirrord.json", "--", "/app/capabilities-rust-backend"]
