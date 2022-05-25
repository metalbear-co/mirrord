# mirrord-cli
mirrord-cli is the actual binary that users use to run mirrord. The client helps you execute processes with mirrord injected to it.
Right now injection is done using `LD_PRELOAD` on Linux and `DYLD_INSERT_LIBRARIES` on macOS.

## Usage
`mirrord exec --pod-name <POD_NAME> <BINARY> [BINARY_ARGS..]`

## Compatibility
### Linux
`LD_PRELOAD` works only on dynamically linked binaries, so static binaries won't work. Most development use cases are dynamic binaries, so it should be okay.
We do have plan to support static binaries, so let us know if you encountered a use case that isn't covered.

### macOS
`DYLD_INSERT_LIBRARIES` works on "unprotected" binaries, which are most development binaries anyway. To be more specific, the following use cases won't work:

* setuid and/or setgid bits are set
* restricted by entitlements
* restricted segment

[Source](https://theevilbit.github.io/posts/dyld_insert_libraries_dylib_injection_in_macos_osx_deep_dive/)

Please let us know if you encountered a use case where it doesn't work for you, of whether it's documented that it isn't supported, so we know there's demand to implement that use case.