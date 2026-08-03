FROM ubuntu:24.04 AS base

ARG TARGETARCH

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    GOPATH=/go \
    GOMODCACHE=/go/pkg/mod \
    GOCACHE=/go/cache \
    SQLX_OFFLINE=true \
    DEBIAN_FRONTEND=noninteractive
ENV PATH=$PATH:/usr/local/cargo/bin:/usr/local/go/bin:/go/bin
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

ARG GO_MINORS="1.24 1.25 1.26"

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

ENV UV_TOOL_BIN_DIR=/usr/local/bin

RUN set -eux; \
    curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh; \
    uv tool install --no-cache "cargo-zigbuild==${CARGO_ZIGBUILD_VERSION}"; \
    uv tool install --no-cache "ziglang==${ZIGLANG_VERSION}" --with-executables-from ziglang; \
    ln -sf "$(command -v python-zig)" /usr/local/bin/zig; \
    cargo-zigbuild --version; \
    zig version

# `mysqldump`/`mysqladmin` must be MySQL's own binaries and `mariadb-dump`/`mariadb-admin` MariaDB's.
ARG MONGO_TOOLS_RELEASE=8.0
ARG COCKROACH_RELEASE=v26.2.5
ARG CLICKHOUSE_RELEASE=26.3.17.56
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends gnupg mariadb-client postgresql-client; \
    apt-get download mysql-client-core-8.0; \
    for bin in mysqldump mysqladmin; do \
        dpkg-deb --fsys-tarfile mysql-client-core-8.0_*.deb \
            | tar -xO "./usr/bin/$bin" > "/usr/local/bin/$bin"; \
        chmod +x "/usr/local/bin/$bin"; \
    done; \
    rm -f mysql-client-core-8.0_*.deb; \
    curl -fsSL "https://www.mongodb.org/static/pgp/server-${MONGO_TOOLS_RELEASE}.asc" \
        | gpg --dearmor -o /usr/share/keyrings/mongodb-server.gpg; \
    echo "deb [signed-by=/usr/share/keyrings/mongodb-server.gpg] https://repo.mongodb.org/apt/ubuntu noble/mongodb-org/${MONGO_TOOLS_RELEASE} multiverse" \
        > /etc/apt/sources.list.d/mongodb-org.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends mongodb-database-tools; \
    curl -fsSL "https://binaries.cockroachdb.com/cockroach-${COCKROACH_RELEASE}.linux-${TARGETARCH}.tgz" \
        | tar -xz --strip-components=1 -C /usr/local/bin --wildcards '*/cockroach'; \
    curl -fsSL "https://github.com/ClickHouse/ClickHouse/releases/download/v${CLICKHOUSE_RELEASE}-lts/clickhouse-common-static-${CLICKHOUSE_RELEASE}-${TARGETARCH}.tgz" \
        | tar -xz --strip-components=3 -C /usr/local/bin \
            "clickhouse-common-static-${CLICKHOUSE_RELEASE}/usr/bin/clickhouse"; \
    ln -sf /usr/local/bin/clickhouse /usr/local/bin/clickhouse-client; \
    rm -rf /var/lib/apt/lists/*; \
    for bin in mysqldump mysqladmin; do \
        if "$bin" --version | grep -qi mariadb; then \
            echo "$bin resolves to MariaDB's alias, not MySQL's binary" >&2; \
            exit 1; \
        fi; \
    done; \
    mariadb-dump --version | grep -qi mariadb; \
    mariadb-admin --version | grep -qi mariadb; \
    mongodump --version; clickhouse-client --version; cockroach version; pg_dump --version

ARG KUBECTL_MINOR=1.36
ARG MINIKUBE_MAJOR=v1
ARG HELM_RELEASE=v4.2.3

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
    curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-4 \
        | bash -s -- --version "${HELM_RELEASE}"; \
    kubectl version --client && helm version --short && minikube version


COPY rust-toolchain.toml ./
RUN rustup show && rustc --version

# The bind-mounted /workspace is owned by the host user, not root.
RUN git config --system --add safe.directory '*'

ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

FROM base AS apps

WORKDIR /src
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    set -eux; \
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
