FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:75752d06e0de748b7bc5e5b1a54575e3c968f5cf AS chef
ARG TARGETARCH
WORKDIR /app

COPY ./mirrord/agent/platform.sh /app/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# cargo-chef 0.1.69 breaks the build
RUN cargo install cargo-chef@0.1.72

FROM chef AS planner

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

RUN cargo +nightly-2025-08-01 chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo +nightly-2025-08-01 chef cook --release --zigbuild --target $(cat /.platform) --recipe-path recipe.json

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

RUN cargo +nightly-2025-08-01 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release
RUN cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:0579bb32706961fec809f1fbdc728e72041366a6
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]
