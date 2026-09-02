ARG RELEASE=1

FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:54901618bd7bdc9b4975fb7e5439e519f6469ac0 AS builder
ARG TARGETARCH
ARG RELEASE
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
COPY mirrord/intproxy/protocol /app/mirrord/intproxy/protocol
COPY mirrord/remote/layer/protocol /app/mirrord/remote/layer/protocol
COPY mirrord/agent /app/mirrord/agent
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/

# workspace files requires all members to exist while most of those aren't really needed
# so we remove from the workspace. `default-members` goes first: it names crates not copied here,
# and its `members` substring would otherwise make the members rewrite match it too.
RUN sed -i '/^default-members = \[/,/^\]/d' /app/Cargo.toml && \
    sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n    "mirrord/sessions-manager/client",\n    "mirrord/sessions-manager/protocol",\n    "mirrord/intproxy/protocol",\n    "mirrord/remote/layer/protocol",\n]' /app/Cargo.toml && \
    sed -i '/exclude = \[/,/\]/c\exclude = [\n    "mirrord/sessions-manager",\n    "mirrord/intproxy",\n    "mirrord/layer",\n    "mirrord/remote",\n]' /app/Cargo.toml


# Build final binary against BuildKit cache mounts. The target directory is one of them, so cargo's
# own fingerprints already skip everything unchanged since the last build on this builder.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/app/target,id=target-agent-$TARGETARCH,sharing=locked \
    if [ "$RELEASE" = "1" ]; then release_flag=--release; target_dir=release; else release_flag=; target_dir=debug; fi; \
    cargo +nightly-2026-08-13 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) ${release_flag:-} && \
    # Copy binary out of cached target directory
    cp "/app/target/$(cat /.platform)/${target_dir}/mirrord-agent" /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:54901618bd7bdc9b4975fb7e5439e519f6469ac0 AS agent
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]

FROM builder AS remote-bootstrap-builder

COPY mirrord/layer-lib /app/mirrord/layer-lib
COPY mirrord/sip /app/mirrord/sip
COPY mirrord/layer/core /app/mirrord/layer/core
COPY mirrord/layer/macro /app/mirrord/layer/macro
COPY mirrord/config /app/mirrord/config
COPY mirrord/str-win /app/mirrord/str-win
COPY mirrord/utils-win /app/mirrord/utils-win
COPY mirrord/analytics /app/mirrord/analytics
COPY mirrord/jaq /app/mirrord/jaq
COPY mirrord/test-macros /app/mirrord/test-macros
COPY mirrord/progress /app/mirrord/progress
COPY mirrord/console /app/mirrord/console
COPY mirrord/remote/bootstrap /app/mirrord/remote/bootstrap
COPY mirrord/remote/layer /app/mirrord/remote/layer

RUN sed -i '/members = \[/,/\]/c\members = [\n    "mirrord/*",\n    "mirrord/sessions-manager/client",\n    "mirrord/sessions-manager/protocol",\n    "mirrord/intproxy/protocol",\n    "mirrord/layer/core",\n    "mirrord/layer/macro",\n    "mirrord/remote/bootstrap",\n    "mirrord/remote/layer",\n    "mirrord/remote/layer/protocol",\n]' /app/Cargo.toml

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/app/target,id=target-agent-$TARGETARCH,sharing=locked \
    if [ "$RELEASE" = "1" ]; then release_flag=--release; target_dir=release; else release_flag=; target_dir=debug; fi; \
    cargo +nightly-2026-08-13 zigbuild --manifest-path /app/mirrord/remote/layer/Cargo.toml --target $(cat /.platform) ${release_flag:-} && \
    cp "/app/target/$(cat /.platform)/${target_dir}/libmirrord_remote_layer.so" /mirrord-remote-layer.so

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git-agent-$TARGETARCH,sharing=locked \
    --mount=type=cache,target=/app/target,id=target-agent-$TARGETARCH,sharing=locked \
    if [ "$RELEASE" = "1" ]; then release_flag=--release; target_dir=release; else release_flag=; target_dir=debug; fi; \
    MIRRORD_AGENT_BINARY=/mirrord-agent \
    MIRRORD_REMOTE_LAYER_BINARY=/mirrord-remote-layer.so \
    cargo +nightly-2026-08-13 zigbuild --manifest-path /app/mirrord/remote/bootstrap/Cargo.toml --target $(cat /.platform) ${release_flag:-} && \
    cp "/app/target/$(cat /.platform)/${target_dir}/libmirrord_remote_bootstrap.so" /mirrord-remote-bootstrap.so

FROM debian:bookworm-slim AS remote-bootstrap
COPY --from=remote-bootstrap-builder /mirrord-remote-bootstrap.so /opt/mirrord/lib/libmirrord_remote_bootstrap.so
