FROM buildpack-deps:bookworm AS base

RUN apt-get update && apt-get install -y protobuf-compiler
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
COPY rust-toolchain.toml .
RUN ./rustup-init.sh -y --default-toolchain none
ENV PATH="$PATH:/root/.cargo/bin"
RUN rustup toolchain install
