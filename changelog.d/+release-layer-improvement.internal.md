Don't build builder image as part of the build, use a prebuilt image - improve cd time
Use `taiki-e/install-action` instead of `cargo install` (compiles from source) for installing `cross`.