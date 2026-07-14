FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:54901618bd7bdc9b4975fb7e5439e519f6469ac0 AS builder
ARG TARGETARCH
WORKDIR /app

COPY ./mirrord/agent/platform.sh /app/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Install mold linker for faster linking
RUN apt-get update && apt-get install -y mold && rm -rf /var/lib/apt/lists/*

# Use mold linker for faster linking
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"



# Copy order is important here, since we want the cache to be invalidated as less as possible
# so we start with the most static files, then the most dynamic ones
COPY .cargo /app/.cargo
COPY mirrord/macros /app/mirrord/macros
COPY mirrord/nightly-polyfill /app/mirrord/nightly-polyfill
COPY mirrord/tls-util /app/mirrord/tls-util
COPY mirrord/protocol /app/mirrord/protocol
COPY mirrord/protocol-io /app/mirrord/protocol-io
COPY mirrord/sessions-manager/client /app/mirrord/sessions-manager/client
COPY mirrord/sessions-manager/protocol /app/mirrord/sessions-manager/protocol
COPY mirrord/agent /app/mirrord/agent
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/

# workspace files requires all members to exist while most of those aren't really needed
# so we remove from the workspace. `default-members` goes first: it names crates not copied here,
# and its `members` substring would otherwise make the members rewrite match it too.
RUN sed -i '/^default-members = \[/,/^\]/d' /app/Cargo.toml && \
    sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n    "mirrord/sessions-manager/client",\n    "mirrord/sessions-manager/protocol",\n]' /app/Cargo.toml


# Build final binary against BuildKit cache mounts. The target directory is one of them, so cargo's
# own fingerprints already skip everything unchanged since the last build on this builder.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/app/target,id=target-agent-$TARGETARCH,sharing=locked \
    cargo +nightly-2026-08-13 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release && \
    # Copy binary out of cached target directory
    cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:54901618bd7bdc9b4975fb7e5439e519f6469ac0
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]
