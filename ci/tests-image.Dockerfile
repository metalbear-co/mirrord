# syntax=docker/dockerfile:1.7

FROM golang:1.24-bookworm AS go124

FROM golang:1.25-bookworm AS go125

FROM golang:1.26-bookworm AS go126

FROM node:24-bookworm

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG RUST_TOOLCHAIN=nightly-2026-02-24

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_TARGET_DIR=/workspace/target \
    MIRRORD_LAYER_FILE=/workspace/target/debug/libmirrord_layer.so \
    MIRRORD_TELEMETRY=false \
    MIRRORD_TESTS_USE_BINARY=/workspace/target/debug/mirrord \
    PATH=/root/.cargo/bin:/opt/go/1.24/bin:/opt/go/1.25/bin:/opt/go/1.26/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    clang \
    curl \
    git \
    libclang-dev \
    libssl-dev \
    pkg-config \
    protobuf-compiler \
    python3 \
    python3-pip \
    python3-venv \
    && rm -rf /var/lib/apt/lists/*

COPY --from=go124 /usr/local/go /opt/go/1.24
COPY --from=go125 /usr/local/go /opt/go/1.25
COPY --from=go126 /usr/local/go /opt/go/1.26

RUN --mount=type=cache,target=/root/.cargo/registry,id=mirrord-tests-cargo-registry \
    --mount=type=cache,target=/root/.cargo/git,id=mirrord-tests-cargo-git \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \
    -y \
    --profile minimal \
    --default-toolchain "${RUST_TOOLCHAIN}" \
    --component rustfmt \
    --component clippy \
    --component rustc \
    && rustup target add x86_64-unknown-linux-gnu \
    && cargo install cargo-nextest --locked

WORKDIR /workspace

COPY . /workspace/

RUN python3 -m pip install --break-system-packages flask fastapi 'uvicorn[standard]' \
    && npm install --no-save --no-package-lock express

RUN set -eux; \
    rustc_apps=( \
        mirrord/layer-tests/tests/apps/issue1123 \
        mirrord/layer-tests/tests/apps/issue1054 \
        mirrord/layer-tests/tests/apps/issue1458 \
        mirrord/layer-tests/tests/apps/issue1458portnot53 \
        mirrord/layer-tests/tests/apps/issue2058 \
        mirrord/layer-tests/tests/apps/issue2204 \
    ); \
    for app in "${rustc_apps[@]}"; do \
        mkdir -p "/workspace/${app}/target"; \
        rustc "/workspace/${app}/"*.rs --out-dir "/workspace/${app}/target"; \
    done

RUN set -eux; \
    cd /workspace/mirrord/layer-tests/tests; \
    export PATH="/opt/go/1.24/bin:$PATH"; \
    go version; \
    ../../../scripts/build_go_apps.sh 24; \
    export PATH="/opt/go/1.25/bin:$PATH"; \
    go version; \
    ../../../scripts/build_go_apps.sh 25; \
    export PATH="/opt/go/1.26/bin:$PATH"; \
    go version; \
    ../../../scripts/build_go_apps.sh 26

RUN export PATH="/opt/go/1.26/bin:$PATH" \
    && ./mirrord/layer-tests/tests/apps/dlopen_cgo/build_test_app.sh \
    && ./scripts/build_c_apps.sh

RUN --mount=type=cache,target=/root/.cargo/registry,id=mirrord-tests-cargo-registry \
    --mount=type=cache,target=/root/.cargo/git,id=mirrord-tests-cargo-git \
    --mount=type=cache,target=/cargo-target,id=mirrord-tests-target \
    set -eux; \
    export CARGO_TARGET_DIR=/cargo-target; \
    cargo_apps=( \
        mirrord/layer-tests/tests/apps/fileops \
        mirrord/layer-tests/tests/apps/outgoing \
        mirrord/layer-tests/tests/apps/recv_from \
        mirrord/layer-tests/tests/apps/dns_resolve \
        mirrord/layer-tests/tests/apps/listen_ports \
        mirrord/layer-tests/tests/apps/issue1776 \
        mirrord/layer-tests/tests/apps/issue1776portnot53 \
        mirrord/layer-tests/tests/apps/issue1899 \
        mirrord/layer-tests/tests/apps/issue2001 \
        mirrord/layer-tests/tests/apps/issue2438 \
        mirrord/layer-tests/tests/apps/issue3248 \
        mirrord/layer-tests/tests/apps/rebind0 \
        mirrord/layer-tests/tests/apps/dup_listen \
        mirrord/layer-tests/tests/apps/double_listen \
    ); \
    for app in "${cargo_apps[@]}"; do \
        cargo build --manifest-path "/workspace/${app}/Cargo.toml"; \
    done; \
    cargo build -p mirrord-layer -p mirrord; \
    cargo test -p mirrord-layer-tests --no-default-features --no-run; \
    rm -rf /workspace/target; \
    mkdir -p /workspace/target; \
    cp -a /cargo-target/. /workspace/target/

CMD ["cargo", "nextest", "run", "-p", "mirrord-layer-tests", "--no-default-features"]
