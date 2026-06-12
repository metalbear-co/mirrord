FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.toml
COPY backend/Cargo.toml backend/Cargo.toml
COPY backend/src backend/src
RUN cargo build -p capabilities-rust-backend --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/capabilities-rust-backend /app/capabilities-rust-backend

# local dependencies image contains these paths
#COPY --from=mirrord-deps-builder:local /opt/mirrord/bin/mirrord /opt/mirrord/mirrord
#COPY --from=mirrord-deps-builder:local /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/libmirrord_layer.so
COPY --from=mirrord-deps-builder:local /opt/mirrord/bin/mirrord-agent /opt/mirrord/mirrord-agent
COPY --from=mirrord-deps-builder:local /opt/mirrord/lib/libmirrord_serverless_bootstrap.so /opt/mirrord/libmirrord_serverless_bootstrap.so

ENV MIRRORD_SESSIONS_MANAGER_URL=host.docker.internal:4971
ENV MIRRORD_SESSION_ID=demo
ENTRYPOINT ["/app/capabilities-rust-backend"]
