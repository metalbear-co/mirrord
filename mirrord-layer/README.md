# mirrord-layer

This part of [mirrord](https://github.com/metalbear-co/mirrord) is preloaded into a process and hooks libc functions, making them work with [mirrord-agent](https://github.com/metalbear-co/mirrord-agent) rather than locally.

To preload mirrord-layer into a process:
* On Linux, run `LD_PRELOAD=<path-to-build-output>/mirrord-layer.so <path-to-process>`
* On macOS, run `DYLD_INSERT_LIBRARIES=<path-to-build-output>/mirrord-layer.dylib <path-to-process>`

# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Contributing
Anyone is welcome to contribute. Please feel free to create PRs with proper documentation, formatting and changelog record.

## License
mirrord-layer is MIT licensed.