# Using the agent's build image, that has rustup targets, zigbuild etc.
FROM --platform=$BUILDPLATFORM ghcr.io/metalbear-co/ci-agent-build:602d397f8e2088ca6ef229e7da0f86490153d8b7 AS build-env
ARG TARGETARCH

WORKDIR /build
COPY . .

# Translate docker's platform to cargo's target.
RUN /build/platform.sh


# Build binary.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN cargo zigbuild -Z bindeps --target $(cat /.platform)
RUN cp /build/target/$(cat /.platform)/debug/rust-sqs-printer /rust-sqs-printer

# Slim final layer with only the binary.
FROM debian:stable-slim
# TODO: optimize this?
RUN apt-get update
RUN apt-get install -y --no-install-recommends ca-certificates
RUN update-ca-certificates

WORKDIR /app
COPY --from=build-env /rust-sqs-printer .

CMD ["./rust-sqs-printer"]

