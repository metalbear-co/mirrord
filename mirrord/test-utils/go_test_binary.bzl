"""`go_test_binary`, a Go test app built with each Go version that the tests run it with.

The layer hooks the syscalls that Go's runtime makes itself, and the runtime can change from one Go
version to the next, so the tests run each Go app built with each version in `GO_VERSIONS`.
"""

load("@rules_go//go:def.bzl", "go_binary", "go_cross_binary")

# Keyed by the minor version, because `GoVersion` in the layer and the end-to-end tests names each
# version by it.
# The `go_sdk` extension must download an SDK for each version.
GO_VERSIONS = {
    "25": "1.25",
    "26": "1.26",
    "27": "1.27",
}

def go_test_binary(
        name,
        srcs = ["main.go"],
        binary_name = None,
        deps = [],
        platform = None,
        versions = GO_VERSIONS.keys(),
        target_compatible_with = [],
        visibility = None):
    """Builds the Go app in `srcs` once for each version in `versions`.

    Each build is a `go_cross_binary` named `<binary_name>.<minor>`, like `dir-go.25`, which the
    tests run the app by, and `name` is a filegroup of the builds, which a test lists.

    The app builds with cgo, as `go build` does, so a program that uses `net` links dynamically and
    the layer can be preloaded into it. On Windows, it builds without cgo, as `go build` does on a
    machine without a C compiler. It keeps its symbol table, because the layer finds Go's runtime by
    its symbols, and the build passes `--strip=never` so that rules_go doesn't strip it.

    Args:
      name: The name of the filegroup of the builds.
      srcs: The app's sources, `main.go` by default.
      binary_name: The name of the builds, before the version. By default, `name`.
      deps: The app's Go dependencies.
      platform: The platform to build the app for, if not the build's own.
      versions: The minor versions of `GO_VERSIONS` that the tests run the app with, all of them by
        default.
      target_compatible_with: The platforms of the tests that run the app.
      visibility: The visibility of the filegroup and of each build.
    """
    binary_name = binary_name or name

    go_binary(
        name = name + "_app",
        srcs = srcs,
        cgo = True,
        pure = select({
            "@platforms//os:windows": "on",
            "//conditions:default": "off",
        }),
        deps = deps,
        # Only the builds for each Go version run.
        tags = ["manual"],
        target_compatible_with = target_compatible_with,
    )

    for minor in versions:
        go_cross_binary(
            name = "{}.{}".format(binary_name, minor),
            platform = platform,
            sdk_version = GO_VERSIONS[minor],
            target = name + "_app",
            target_compatible_with = target_compatible_with,
            visibility = visibility,
        )

    native.filegroup(
        name = name,
        srcs = ["{}.{}".format(binary_name, minor) for minor in versions],
        target_compatible_with = target_compatible_with,
        visibility = visibility,
    )
