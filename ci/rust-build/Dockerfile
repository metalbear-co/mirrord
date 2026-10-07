FROM buildpack-deps:bookworm AS base

RUN apt-get update && apt-get install -y protobuf-compiler
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
RUN ./rustup-init.sh -y -c rustfmt --default-toolchain 1.99.0
ENV PATH="$PATH:/root/.cargo/bin"
