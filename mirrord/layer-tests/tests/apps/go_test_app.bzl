"""The Go test apps, each built with every Go version that the tests run it with."""

load("@rules_go//go:def.bzl", "go_binary", "go_cross_binary")

# The Go versions that each Go app is built with, keyed by the minor version, as `GoVersion` names
# each version in the layer tests (`tests/common/mod.rs`) and the end-to-end tests
# (`mirrord/tests/src/utils/application.rs`). `apps.MODULE.bazel` downloads them.
GO_VERSIONS = {
    "25": "1.25",
    "26": "1.26",
    "27": "1.27",
}

def go_test_app(
        directory = "",
        *,
        target_compatible_with,
        deps = [],
        platform = None,
        visibility = None):
    """Builds the Go app in `directory` as `<directory>/<minor>.go_test_app` for each Go version.

    The names are the paths that the tests run the apps from. The app builds as `go build` builds
    it: with cgo where Go supports it, so that a program that uses `net` is linked dynamically and
    the layer can be preloaded into it, and with its symbol table, which the layer finds Go's
    runtime by (see `--strip` in `.bazelrc`).

    Args:
      directory: The app's directory, relative to the package. By default, the package's own.
      target_compatible_with: The platforms that the tests that run the app run on. On Windows,
        the app builds without cgo, as `go build` builds it there without a C compiler.
      deps: The Go packages that the app imports, other than the standard library's.
      platform: The platform to build the app for. By default, the build's own.
      visibility: The visibility of the app's builds, which the tests that run them need.
    """
    prefix = directory + "/" if directory else ""

    go_binary(
        name = prefix + "app",
        srcs = native.glob([prefix + "*.go"]),
        cgo = True,
        pure = select({
            "@platforms//os:windows": "on",
            "//conditions:default": "off",
        }),
        # Only the Go versions below build it, so `//...` doesn't build it a fourth time with the
        # default SDK.
        tags = ["manual"],
        target_compatible_with = target_compatible_with,
        deps = deps,
    )

    for minor, version in GO_VERSIONS.items():
        go_cross_binary(
            name = "{}{}.go_test_app".format(prefix, minor),
            platform = platform,
            sdk_version = version,
            target = prefix + "app",
            target_compatible_with = target_compatible_with,
            visibility = visibility,
        )
