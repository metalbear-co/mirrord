FROM ubuntu:24.04 AS base

ARG TARGETARCH

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    GOPATH=/go \
    GOMODCACHE=/go/pkg/mod \
    GOCACHE=/go/cache \
    SQLX_OFFLINE=true \
    DEBIAN_FRONTEND=noninteractive
ENV PATH=/usr/local/cargo/bin:/usr/local/go/bin:/go/bin:$PATH
ENV PROTOC=/usr/local/bin/protoc \
    PROTOC_INCLUDE=/usr/local/include

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        jq \
        libclang-dev \
        libsasl2-dev \
        libssl-dev \
        libzstd-dev \
        mold \
        pkg-config \
        python3 \
        python3-flask \
        python3-pip \
        unzip \
        xz-utils \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN pip3 install --break-system-packages --no-cache-dir fastapi==0.138.0 'uvicorn[standard]==0.49.0'

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --default-toolchain none --profile minimal

WORKDIR /workspace

ARG PROTOC_VERSION=35.1

RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) parch=x86_64 ;; \
        arm64) parch=aarch_64 ;; \
        *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/protoc-${PROTOC_VERSION}-linux-${parch}.zip" -o /tmp/protoc.zip; \
    unzip -o /tmp/protoc.zip -d /usr/local 'bin/*' 'include/*'; \
    rm /tmp/protoc.zip; \
    chmod +x /usr/local/bin/protoc; \
    protoc --version

ARG NEXTEST_MAJOR=0.9

RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) slug=linux ;; \
        arm64) slug=linux-arm ;; \
        *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://get.nexte.st/${NEXTEST_MAJOR}/${slug}" | tar -C /usr/local/cargo/bin -xz; \
    cargo-nextest nextest --version

ARG SCCACHE_VERSION=0.12.0

RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) starch=x86_64 ;; \
        arm64) starch=aarch64 ;; \
        *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${starch}-unknown-linux-musl.tar.gz" \
        | tar -xz --strip-components=1 -C /usr/local/cargo/bin "sccache-v${SCCACHE_VERSION}-${starch}-unknown-linux-musl/sccache"; \
    chmod +x /usr/local/cargo/bin/sccache; \
    sccache --version

ENV RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/root/.cache/sccache

ARG GO_MINORS="1.23 1.24 1.25 1.26"

RUN set -eux; \
    mkdir -p /usr/local/go-versions; \
    arch="$(dpkg --print-architecture)"; \
    for minor in $GO_MINORS; do \
        ver="$(curl -fsSL 'https://go.dev/dl/?mode=json&include=all' \
            | jq -r --arg m "go${minor}" \
                'map(select(.stable and ((.version|startswith($m+".")) or (.version==$m)))) | .[0].version')"; \
        test -n "$ver" && test "$ver" != "null"; \
        echo "Installing ${ver} for ${minor}"; \
        curl -fsSL "https://go.dev/dl/${ver}.linux-${arch}.tar.gz" | tar -C /tmp -xz; \
        mv /tmp/go "/usr/local/go-versions/${minor}"; \
    done; \
    ln -s "/usr/local/go-versions/$(echo $GO_MINORS | tr ' ' '\n' | tail -1)" /usr/local/go; \
    go version

ARG NODE_MAJOR=24
ARG PNPM_MAJOR=11

RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) na=x64 ;; \
        arm64) na=arm64 ;; \
        *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    ver="$(curl -fsSL https://nodejs.org/dist/index.json | jq -r --arg M "v${NODE_MAJOR}." '[.[]|.version|select(startswith($M))][0]')"; \
    test -n "$ver" && test "$ver" != "null"; \
    curl -fsSL "https://nodejs.org/dist/${ver}/node-${ver}-linux-${na}.tar.xz" | tar -C /usr/local --strip-components=1 -xJ; \
    corepack enable && corepack prepare "pnpm@${PNPM_MAJOR}" --activate; \
    node --version && pnpm --version

ARG CARGO_ZIGBUILD_VERSION=0.22.1
ARG ZIGLANG_VERSION=0.15.2

# `cargo xtask build-cli` builds the CLI and layer through `cargo zigbuild`, targeting an old glibc
# so the artifacts stay portable.
ENV UV_TOOL_BIN_DIR=/usr/local/bin

RUN set -eux; \
    curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh; \
    uv tool install --no-cache "cargo-zigbuild==${CARGO_ZIGBUILD_VERSION}"; \
    uv tool install --no-cache "ziglang==${ZIGLANG_VERSION}" --with-executables-from ziglang; \
    ln -sf "$(command -v python-zig)" /usr/local/bin/zig; \
    cargo-zigbuild --version; \
    zig version

ARG KUBECTL_MINOR=1.36
ARG MINIKUBE_MAJOR=v1

RUN set -eux; \
    kver="$(curl -fsSL "https://dl.k8s.io/release/stable-${KUBECTL_MINOR}.txt")"; \
    curl -fsSL "https://dl.k8s.io/release/${kver}/bin/linux/${TARGETARCH}/kubectl" -o /usr/local/bin/kubectl; \
    chmod +x /usr/local/bin/kubectl; \
    mver="$(curl -fsSL https://storage.googleapis.com/minikube/releases.json \
        | jq -r --arg M "${MINIKUBE_MAJOR}." '[.[]|.name|select(startswith($M))][0]')"; \
    test -n "$mver" && test "$mver" != "null"; \
    curl -fsSL "https://storage.googleapis.com/minikube/releases/${mver}/minikube-linux-${TARGETARCH}" \
        -o /usr/local/bin/minikube; \
    chmod +x /usr/local/bin/minikube; \
    curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash; \
    kubectl version --client && helm version --short && minikube version


COPY rust-toolchain.toml ./
RUN rustup show && rustc --version

# The bind-mounted /workspace is owned by the host user, not root; without this git refuses to read
# it ("dubious ownership"), which breaks Go's VCS stamping during `go build`.
RUN git config --system --add safe.directory '*'

ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

# Compiles the test apps. Their sources are only present in this stage; the final image ships the
# compiled apps alone, and the suites bring their own sources by mounting a checkout.
FROM base AS apps

WORKDIR /src
COPY . .

RUN set -eux; \
    scripts/prepare_e2e.sh --apps-only; \
    mkdir -p /artifacts/target/debug; \
    find . \( -name '*.go_test_app' -o -name '*.c_test_app' \) \
        | tar -cf - -T - | tar -xf - -C /artifacts; \
    if [ -d node_modules ]; then tar -cf - node_modules | tar -xf - -C /artifacts; fi; \
    for dir in $(find tests/*/* -name Cargo.toml -printf '%h\n'); do \
        name="$(sed -n 's/^name = "\(.*\)"/\1/p' "${dir}/Cargo.toml" | head -1)"; \
        cp "target/debug/${name}" /artifacts/target/debug/; \
    done

FROM base AS final

COPY --from=apps /artifacts /opt/e2e-artifacts
COPY tests/e2e-entrypoint.sh /usr/local/bin/e2e-entrypoint

ENV KUBECONFIG=/root/.kube/config
WORKDIR /workspace
ENTRYPOINT ["e2e-entrypoint"]
CMD ["bash"]

LABEL org.opencontainers.image.source="https://github.com/metalbear-co/mirrord"
LABEL org.opencontainers.image.description="mirrord and mirrord operator E2E runner"
