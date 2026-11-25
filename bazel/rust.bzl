"""`crate_package` for mirrord's own crates: the macros of `crate_package.bzl`, with mirrord's packages."""

load("@cargo_packages//:packages.bzl", "PACKAGES")
load(":crate_package.bzl", _crate_package = "crate_package")

def crate_package(crate_data):  # buildifier: disable=unnamed-macro
    """Returns the macros for the Rust targets of the calling BUILD file's Cargo package.

    Args:
      crate_data: What `crate_data()` returns in `@crates//<name>:data.bzl`, where `<name>` is the
        `name` that the package's `Cargo.toml` gives it.

    Returns:
      The macros, as `crate_package` in `crate_package.bzl` describes them.
    """
    return _crate_package(crate_data, PACKAGES, Label("@cargo_packages//:metadata.json"))
