# syntax=docker/dockerfile:1.7

FROM golang:1.24-bookworm AS go124-build

WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc \
    && rm -rf /var/lib/apt/lists/*

COPY . /workspace/

RUN --mount=type=cache,target=/go/pkg/mod,id=mirrord-tests-go124-mod \
    --mount=type=cache,target=/root/.cache/go-build,id=mirrord-tests-go124-build \
    set -eux; \
    ./scripts/build_go_apps.sh 24; \
    mkdir -p /go-test-apps; \
    find /workspace -name '24.go_test_app' -exec sh -c '\
        relative_path="${1#/workspace/}"; \
        mkdir -p "/go-test-apps/$(dirname "$relative_path")"; \
        cp "$1" "/go-test-apps/$relative_path"' sh {} \; && \
    touch /go-stage-complete

FROM golang:1.25-bookworm AS go125-build

WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc \
    && rm -rf /var/lib/apt/lists/*

COPY --from=go124-build /go-stage-complete /tmp/go-stage-complete
COPY . /workspace/

RUN --mount=type=cache,target=/go/pkg/mod,id=mirrord-tests-go125-mod \
    --mount=type=cache,target=/root/.cache/go-build,id=mirrord-tests-go125-build \
    set -eux; \
    ./scripts/build_go_apps.sh 25; \
    mkdir -p /go-test-apps; \
    find /workspace -name '25.go_test_app' -exec sh -c '\
        relative_path="${1#/workspace/}"; \
        mkdir -p "/go-test-apps/$(dirname "$relative_path")"; \
        cp "$1" "/go-test-apps/$relative_path"' sh {} \; && \
    touch /go-stage-complete

FROM golang:1.26-bookworm AS go126-build

WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY --from=go125-build /go-stage-complete /tmp/go-stage-complete
COPY . /workspace/

RUN --mount=type=cache,target=/go/pkg/mod,id=mirrord-tests-go126-mod \
    --mount=type=cache,target=/root/.cache/go-build,id=mirrord-tests-go126-build \
    set -eux; \
    ./scripts/build_go_apps.sh 26; \
    mkdir -p /go-test-apps; \
    find /workspace -name '26.go_test_app' -exec sh -c '\
        relative_path="${1#/workspace/}"; \
        mkdir -p "/go-test-apps/$(dirname "$relative_path")"; \
        cp "$1" "/go-test-apps/$relative_path"' sh {} \;

RUN --mount=type=cache,target=/go/pkg/mod,id=mirrord-tests-go126-mod \
    --mount=type=cache,target=/root/.cache/go-build,id=mirrord-tests-go126-build \
    set -eux; \
    ./mirrord/layer-tests/tests/apps/dlopen_cgo/build_test_app.sh; \
    mkdir -p /go-test-apps/mirrord/layer-tests/tests/apps/dlopen_cgo; \
    cp /workspace/mirrord/layer-tests/tests/apps/dlopen_cgo/libgo_server.so \
        /go-test-apps/mirrord/layer-tests/tests/apps/dlopen_cgo/; \
    cp /workspace/mirrord/layer-tests/tests/apps/dlopen_cgo/out.cpp_dlopen_cgo \
        /go-test-apps/mirrord/layer-tests/tests/apps/dlopen_cgo/; \
    touch /go-stage-complete

FROM node:24-bookworm

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG RUST_TOOLCHAIN=nightly-2026-02-24
ARG CARGO_BUILD_JOBS=2

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    CARGO_TARGET_DIR=/workspace/target \
    MIRRORD_TELEMETRY=false \
    PATH=/root/.cargo/bin:$PATH

COPY --from=go126-build /go-stage-complete /tmp/go-stage-complete

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
COPY --from=go124-build /go-test-apps/ /workspace/
COPY --from=go125-build /go-test-apps/ /workspace/
COPY --from=go126-build /go-test-apps/ /workspace/

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

RUN ./scripts/build_c_apps.sh

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
        tests/issue1317 \
        tests/rust-bypassed-unix-socket \
        tests/rust-e2e-fileops \
        tests/rust-sqs-printer \
        tests/rust-unix-socket-client \
        tests/rust-websockets \
    ); \
    for app in "${cargo_apps[@]}"; do \
        cargo build --manifest-path "/workspace/${app}/Cargo.toml"; \
    done; \
    cargo build -p mirrord-layer -p mirrord; \
    touch /workspace/wizard-frontend.tar.gz; \
    cargo build -p mirrord --features wizard; \
    export MIRRORD_LAYER_FILE=/cargo-target/debug/libmirrord_layer.so; \
    export MIRRORD_TESTS_USE_BINARY=/cargo-target/debug/mirrord; \
    cargo test -p mirrord-layer-tests --no-default-features --no-run; \
    cargo test -p mirrord-tests --no-default-features --features "cli targetless job ephemeral" --no-run; \
    rm -rf /workspace/target; \
    mkdir -p /workspace/target; \
    cp -a /cargo-target/. /workspace/target/; \
    rm -rf /workspace/target/debug/incremental /workspace/target/debug/examples

RUN rm -rf \
    /root/.cargo/registry \
    /root/.cargo/git \
    /root/.rustup/downloads \
    /root/.rustup/tmp \
    /root/.npm \
    /root/.cache/pip

ENV MIRRORD_LAYER_FILE=/workspace/target/debug/libmirrord_layer.so \
    MIRRORD_TESTS_USE_BINARY=/workspace/target/debug/mirrord

CMD ["cargo", "nextest", "run", "-p", "mirrord-layer-tests", "--no-default-features"]
