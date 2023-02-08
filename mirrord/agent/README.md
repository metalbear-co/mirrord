# mirrord-agent
[![CI](https://github.com/metalbear-co/mirrord/actions/workflows/ci.yaml/badge.svg)](https://github.com/metalbear-co/mirrord/actions/workflows/ci.yaml)

Agent part of [mirrord](https://github.com/metalbear-co/mirrord) responsible for running on the same node as the debuggee, entering it's namespace and collecting traffic.

mirrord-agent is written in Rust for safety, low memory consumption and performance.

mirrord-agent is distributed as a container image (currently only x86) that is published on [GitHub Packages publicly](https://github.com/metalbear-co/mirrord-agent/pkgs/container/mirrord-agent). 
