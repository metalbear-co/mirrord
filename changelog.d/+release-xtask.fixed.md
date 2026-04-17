Changed the release workflow to build Linux and Windows CLI artifacts through `cargo xtask build-cli` instead of handwritten cargo build steps.
Fixes regression with wrong artifact on Linux causing mirrord to not work
