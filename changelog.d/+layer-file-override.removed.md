Removed the `MIRRORD_LAYER_FILE` override: the CLI always loads its own layer. To run a layer that you built, run the CLI from the same checkout with `bazel run //mirrord/cli`, which builds both.
