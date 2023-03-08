FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:latest as build-env
ARG TARGETARCH
WORKDIR /app

COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/
COPY sample/rust /app/sample/rust
COPY mirrord /app/mirrord
COPY tests /app/tests
COPY .cargo /app/.cargo

RUN ./mirrord/agent/platform.sh
RUN echo $PATH
RUN cargo +nightly-2023-01-31 zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release
RUN cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM ghcr.io/metalbear-co/ci-agent-runtime:latest
COPY --from=build-env /mirrord-agent /

CMD ["./mirrord-agent"]