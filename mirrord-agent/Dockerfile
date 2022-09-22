FROM --platform=$BUILDPLATFORM rustlang/rust:nightly as build-env
RUN apt update && apt install -y libpcap-dev cmake
WORKDIR /app
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/
COPY sample/rust /app/sample/rust
COPY mirrord-auth /app/mirrord-auth
COPY mirrord-protocol /app/mirrord-protocol
COPY mirrord-agent /app/mirrord-agent
COPY mirrord-macro /app/mirrord-macro
COPY mirrord-layer /app/mirrord-layer
COPY mirrord-cli /app/mirrord-cli
COPY tests /app/tests
COPY .cargo /app/.cargo
ARG TARGETPLATFORM
RUN /bin/bash -c 'if [ "$TARGETPLATFORM" = "linux/amd64" ]; then export CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu && rustup target add $CARGO_BUILD_TARGET && cargo +nightly build -Z bindeps --manifest-path /app/mirrord-agent/Cargo.toml --release; fi'
RUN /bin/bash -c 'if [ "$TARGETPLATFORM" = "linux/arm64" ]; then export CARGO_BUILD_TARGET=aarch64-unknown-linux-gnu && rustup target add $CARGO_BUILD_TARGET && cargo +nightly build -Z bindeps --manifest-path /app/mirrord-agent/Cargo.toml --release; fi'
RUN /bin/bash -c 'if [ "$TARGETPLATFORM" = "linux/arm/v7" ]; then export CARGO_BUILD_TARGET=armv7-unknown-linux-gnueabihf && rustup target add $CARGO_BUILD_TARGET && cargo +nightly build -Z bindeps --manifest-path /app/mirrord-agent/Cargo.toml --release; fi'

FROM debian:stable
COPY --from=build-env /app/target/release/mirrord-agent /
RUN apt update && apt install -y libpcap-dev iptables
RUN update-alternatives --set iptables /usr/sbin/iptables-legacy \
    && update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy

CMD ["./mirrord-agent"]