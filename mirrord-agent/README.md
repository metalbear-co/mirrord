# mirrord-agent
[![CI](https://github.com/metalbear-co/mirrord-agent/actions/workflows/ci.yaml/badge.svg)](https://github.com/metalbear-co/mirrord-agent/actions/workflows/ci.yaml)

Agent part of [mirrord](https://github.com/metalbear-co/mirrord) responsible for running on the same node as the debugee, entering it's namespace and collecting traffic.

mirrord-agent is written in Rust for safety, low memory consumption and performance.

mirrord-agent is distributed as a container image (currently only x86) that is published on [GitHub Packages publicly](https://github.com/metalbear-co/mirrord-agent/pkgs/container/mirrord-agent). 

# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Contributing
Anyone is welcome to contribute. Please feel free to create PRs with proper documentation, formatting and changelog record.

## License

mirrord-agent is MIT licensed.