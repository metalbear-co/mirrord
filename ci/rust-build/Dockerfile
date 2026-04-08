FROM buildpack-deps:bookworm AS base

RUN apt-get update && apt-get install -y protobuf-compiler
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
RUN ./rustup-init.sh -y -c rustfmt --default-toolchain nightly-2026-02-24
ENV PATH="$PATH:/root/.cargo/bin"
