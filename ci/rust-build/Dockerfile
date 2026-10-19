FROM buildpack-deps:bookworm AS base

RUN apt-get update && apt-get install -y protobuf-compiler
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
# Keep the toolchain in sync with `rust-toolchain.toml`, which isn't in this image's build context.
RUN ./rustup-init.sh -y -c rustfmt --default-toolchain 1.99.1
ENV PATH="$PATH:/root/.cargo/bin"
