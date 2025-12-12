FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:75752d06e0de748b7bc5e5b1a54575e3c968f5cf AS chef
ARG TARGETARCH
WORKDIR /app

COPY ./mirrord/agent/platform.sh /app/
RUN ./platform.sh

# this takes around 1 minute since libgit2 is slow https://github.com/rust-lang/cargo/issues/9167
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Install sccache for compilation caching with GCS
ENV SCCACHE_VERSION=0.12.0
RUN SCCACHE_ARCH=$(case ${TARGETARCH} in \
    amd64) echo "x86_64" ;; \
    arm64) echo "aarch64" ;; \
    *) echo "x86_64" ;; \
    esac) && \
    curl -fsSL https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl.tar.gz \
    | tar -xzv --strip-components=1 -C /usr/local/bin sccache-v${SCCACHE_VERSION}-${SCCACHE_ARCH}-unknown-linux-musl/sccache && \
    chmod +x /usr/local/bin/sccache

# sccache configuration - will be used if GCS credentials are provided
ENV RUSTC_WRAPPER=/usr/local/bin/sccache

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

# GCS sccache configuration - prefix format: mirrord/{branch}
ARG GCS_BUCKET=""
ARG GCS_RW_MODE="READ_WRITE"
ARG GCS_PATH_PREFIX="mirrord-agent/"

COPY --from=planner /app/recipe.json recipe.json

# Build dependencies with sccache
RUN --mount=type=secret,id=gcp_credentials \
    if [ -n "$GCS_BUCKET" ] && [ -f /run/secrets/gcp_credentials ]; then \
    export SCCACHE_GCS_BUCKET="$GCS_BUCKET"; \
    export SCCACHE_GCS_RW_MODE="$GCS_RW_MODE"; \
    export SCCACHE_GCS_KEY_PREFIX="$GCS_PATH_PREFIX"; \
    export SCCACHE_GCS_KEY_PATH=/run/secrets/gcp_credentials; \
    echo "sccache GCS config: bucket=$GCS_BUCKET prefix=$GCS_PATH_PREFIX"; \
    echo "Credentials file size: $(wc -c < /run/secrets/gcp_credentials) bytes"; \
    echo "Credentials file type check: $(head -c 1 /run/secrets/gcp_credentials)"; \
    else \
    echo "sccache using local disk cache (GCS_BUCKET=$GCS_BUCKET, secret exists: $(test -f /run/secrets/gcp_credentials && echo yes || echo no))"; \
    fi && \
    sccache --stop-server 2>/dev/null || true && \
    if sccache --start-server; then \
    echo "sccache server started"; \
    else \
    echo "sccache failed to start, continuing without cache"; \
    unset RUSTC_WRAPPER; \
    fi && \
    cargo +nightly-2025-08-01 chef cook --release --zigbuild --target $(cat /.platform) --recipe-path recipe.json && \
    echo "sccache stats" && \
    (sccache --show-stats || echo "sccache stats unavailable")

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

# Build final binary with sccache
RUN --mount=type=secret,id=gcp_credentials \
    if [ -n "$GCS_BUCKET" ] && [ -f /run/secrets/gcp_credentials ]; then \
    export SCCACHE_GCS_BUCKET="$GCS_BUCKET"; \
    export SCCACHE_GCS_RW_MODE="$GCS_RW_MODE"; \
    export SCCACHE_GCS_KEY_PREFIX="$GCS_PATH_PREFIX"; \
    export SCCACHE_GCS_KEY_PATH=/run/secrets/gcp_credentials; \
    echo "sccache GCS config: bucket=$GCS_BUCKET prefix=$GCS_PATH_PREFIX"; \
    else \
    echo "sccache using local disk cache"; \
    fi && \
    sccache --stop-server 2>/dev/null || true && \
    if sccache --start-server; then \
    echo "sccache server started"; \
    else \
    echo "sccache failed to start, continuing without cache"; \
    unset RUSTC_WRAPPER; \
    fi && \
    cargo +nightly-2025-08-01 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release && \
    echo "sccache stats" && \
    (sccache --show-stats || echo "sccache stats unavailable")

RUN cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:30dca9bcb32306a028178cac371b1e47e403916c
COPY --from=builder /mirrord-agent /

CMD ["./mirrord-agent"]
