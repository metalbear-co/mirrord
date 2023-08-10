FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:6968c72ab7f403f31e3c1edc68c6d3c4c29b6e27 as chef
ARG TARGETARCH
WORKDIR /app

COPY ./mirrord/agent/platform.sh /app/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

RUN cargo install cargo-chef

FROM chef AS planner

# Copy order is important here, since we want the cache to be invalidated as less as possible
# so we start with the most static files, then the most dynamic ones
COPY .cargo /app/.cargo
COPY mirrord/macros /app/mirrord/macros
COPY mirrord/protocol /app/mirrord/protocol
COPY mirrord/agent /app/mirrord/agent
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/

# workspace files requires all members to exist while most of those aren't really needed
# so we remove from the workspace
RUN sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n]' /app/Cargo.toml

RUN cargo +nightly-2023-04-19 chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo +nightly-2023-04-19 chef cook --release --zigbuild --target $(cat /.platform) --recipe-path recipe.json

# Copy order is important here, since we want the cache to be invalidated as less as possible
# so we start with the most static files, then the most dynamic ones
COPY .cargo /app/.cargo
COPY mirrord/macros /app/mirrord/macros
COPY mirrord/protocol /app/mirrord/protocol
COPY mirrord/agent /app/mirrord/agent
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/

# workspace files requires all members to exist while most of those aren't really needed
# so we remove from the workspace
RUN sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n]' /app/Cargo.toml

RUN cargo +nightly-2023-04-19 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release
RUN cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:latest
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]
