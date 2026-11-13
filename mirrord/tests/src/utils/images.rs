//! Test application images deployed to the cluster by the E2E tests.
//!
//! All images are pinned by digest and deployed with `imagePullPolicy: IfNotPresent`, so pods
//! never race a registry round-trip on creation (`:latest` implies `imagePullPolicy: Always`,
//! which does a GHCR digest check on every pod start and has caused E2E tests to flake on their
//! startup timeout whenever GHCR was slow).
//!
//! To bump an image after publishing a new version, update its digest here:
//!
//! ```sh
//! docker buildx imagetools inspect ghcr.io/metalbear-co/<image>:latest
//! ```

pub const PYTEST_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-pytest@sha256:4882cbdcd240c5a0aaf1f0b8c408729dfba447eb11c187791778a446a3be50da";

pub const TCP_ECHO_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-tcp-echo@sha256:c789fb2cdc91d6ff99deec52d8d05451bf65a6ad617ab7764f7cb82d92664f59";

pub const NODE_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-node@sha256:6e5b34590b4604e6808cf3a01e1db7a26bab8577a3c2de43f480dda6b5b3ad3e";

pub const NODE_UDP_LOGGER_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-node-udp-logger@sha256:acf49e2f77ea2d35655decfd90d2c059fac6984a188e24494f0afb4efaf36ea7";

pub const HTTP_LOGGER_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-http-logger@sha256:578bd796d927b0f0581c349d40a94085155e6ca6c4e0d39d96b749e0623e9f9f";

pub const HTTP_KEEP_ALIVE_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-http-keep-alive@sha256:0e015544035443cdc479a54b7e693759248d8dd80aa26096578dfac9ba0f947d";

pub const WEBSOCKET_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-websocket@sha256:b36e591bbbac6aafee0e4368ad2078f32adca313e5f67bcca1b5c54a237874c5";

pub const UNIX_SOCKET_SERVER_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-unix-socket-server@sha256:4937e4d623c34fe1a904970804e0c30629932770a1e684f6c34ddcf8ca77dc25";

pub const GO_STATFS_IMAGE: &str =
    "ghcr.io/metalbear-co/mirrord-go-statfs@sha256:83429486fc40bf4deaf9f17b968d930f03e92adeefe08bf02e76d69b37a62025";
