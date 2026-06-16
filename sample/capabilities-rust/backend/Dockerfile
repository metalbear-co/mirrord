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
COPY --from=mirrord-deps-builder:local /opt/mirrord/bin/mirrord /opt/mirrord/bin/mirrord
COPY --from=mirrord-deps-builder:local /opt/mirrord/bin/mirrord-agent /opt/mirrord/bin/mirrord-agent
COPY --from=mirrord-deps-builder:local /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so
COPY --from=mirrord-deps-builder:local /opt/mirrord/lib/libmirrord_serverless_bootstrap.so /opt/mirrord/lib/libmirrord_serverless_bootstrap.so
COPY mirrord.json /app/mirrord.json

ENTRYPOINT ["/app/capabilities-rust-backend"]
