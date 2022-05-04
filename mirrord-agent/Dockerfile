FROM rust:latest as build-env
RUN apt update && apt install -y libpcap-dev cmake
RUN rustup component add rustfmt
WORKDIR /app
COPY . /app
RUN cargo build --manifest-path mirrord-agent/Cargo.toml --release

FROM debian:stable
RUN apt update && apt install -y libpcap-dev
COPY --from=build-env /app/target/release/mirrord-agent /

CMD ["./mirrord-agent"]