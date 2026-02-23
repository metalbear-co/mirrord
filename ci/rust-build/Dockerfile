FROM buildpack-deps:bookworm as base

RUN apt-get update && apt-get install -y protobuf-compiler
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
RUN ./rustup-init.sh -y -c rustfmt --default-toolchain nightly-2025-08-01
ENV PATH="$PATH:/root/.cargo/bin"
