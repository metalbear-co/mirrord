# Dockerfile used for building mirrord-layer for x64 with very old libc
# this to support centos7 or Amazon Linux 2.

FROM ghcr.io/cross-rs/x86_64-unknown-linux-gnu:main-centos

RUN yum update -y && \
    yum install centos-release-scl -y && \
    yum install llvm-toolset-7 -y

ENV LIBCLANG_PATH=/opt/rh/llvm-toolset-7/root/usr/lib64/ \
    LIBCLANG_STATIC_PATH=/opt/rh/llvm-toolset-7/root/usr/lib64/ \
    CLANG_PATH=/opt/rh/llvm-toolset-7/root/usr/bin/clang