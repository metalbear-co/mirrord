# mirrord-layer

This part of [mirrord](https://github.com/metalbear-co/mirrord) is preloaded into a process and hooks libc functions, making them work with [mirrord-agent](https://github.com/metalbear-co/mirrord-agent) rather than locally.

To preload mirrord-layer into a process:
* On Linux, run `LD_PRELOAD=<path-to-build-output>/mirrord-layer.so <path-to-process>`
* On macOS, run `DYLD_INSERT_LIBRARIES=<path-to-build-output>/mirrord-layer.dylib <path-to-process>`