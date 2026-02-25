FROM buildpack-deps:bookworm

RUN apt-get update && apt-get install -y protobuf-compiler gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf gcc-x86-64-linux-gnu clang zip
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
RUN chmod +x rustup-init.sh
RUN ./rustup-init.sh -y -c rustfmt --default-toolchain nightly-2025-08-01
ENV PATH="$PATH:/root/.cargo/bin"
RUN rustup target add --toolchain nightly-2025-08-01 x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

RUN apt-get update && apt-get install -y python3-pip
RUN CARGO_NET_GIT_FETCH_WITH_CLI=true python3 -m pip install --break-system-packages cargo-zigbuild
