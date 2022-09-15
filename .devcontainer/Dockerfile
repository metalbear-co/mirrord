# See here for image contents: https://github.com/microsoft/vscode-dev-containers/tree/v0.224.2/containers/rust/.devcontainer/base.Dockerfile

# [Choice] Debian OS version (use bullseye on local arm64/Apple Silicon): buster, bullseye
ARG VARIANT="buster"
FROM mcr.microsoft.com/vscode/devcontainers/rust:0-${VARIANT}

RUN apt update && apt install -y libpcap-dev cmake clang
RUN rustup toolchain install nightly-2022-09-13-x86_64-unknown-linux-gnu && rustup component add rustfmt --toolchain nightly-2022-09-13-x86_64-unknown-linux-gnu && rustup component add clippy --toolchain nightly-2022-09-13-x86_64-unknown-linux-gnu
