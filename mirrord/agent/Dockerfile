FROM --platform=$BUILDPLATFORM instrumentisto/rust:nightly-2023-01-31 as build-env
ARG TARGETARCH
WORKDIR /app
RUN curl -sSL https://bootstrap.pypa.io/get-pip.py -o get-pip.py && python3 get-pip.py
RUN python3 -m pip install ziglang
RUN cargo install cargo-zigbuild
COPY Cargo.toml Cargo.lock CHANGELOG.md README.md LICENSE rust-toolchain.toml /app/
COPY sample/rust /app/sample/rust
COPY mirrord /app/mirrord
COPY tests /app/tests
COPY .cargo /app/.cargo

RUN ./mirrord/agent/platform.sh
RUN rustup component add --toolchain nightly rustfmt
RUN rustup target add --toolchain nightly $(cat /.platform)
RUN apt-get update && apt-get install -y $(cat /.compiler)

RUN cargo +nightly zigbuild -Z bindeps --manifest-path /app/mirrord/agent/Cargo.toml --target $(cat /.platform) --release
RUN cp /app/target/$(cat /.platform)/release/mirrord-agent /mirrord-agent

FROM debian:stable
COPY --from=build-env /mirrord-agent /
RUN apt update && apt install -y iptables
RUN update-alternatives --set iptables /usr/sbin/iptables-legacy \
    && update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy

CMD ["./mirrord-agent"]