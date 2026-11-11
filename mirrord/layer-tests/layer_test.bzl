"""The layer tests, each with the apps and interpreters that it runs (see `layer_tests`)."""

load("//bazel:compatibility.bzl", "LINUX_ONLY", "MACOS_ONLY", "UNIX_ONLY", "WINDOWS_ONLY")
load("//bazel:rust.bzl", "rust_test")
load("//mirrord/layer-tests/tests/apps:go_test_app.bzl", "GO_VERSIONS")

# The package of the test apps. Each app's binary is at the path that `tests/common/mod.rs` runs it
# from (see `tests/apps/BUILD.bazel`).
APPS = "//mirrord/layer-tests/tests/apps"

def c_app(app):
    """Returns the C app `app`."""
    return "{}:{}/out.c_test_app".format(APPS, app)

def rustc_app(app):
    """Returns the single-file Rust app `app`, which isn't a Cargo package."""
    return "{}:{}_app".format(APPS, app)

def go_app(app):
    """Returns the Go app `app` for every Go version that the tests run it with.

    Args:
      app: The app's directory in the apps' package, like `issue2988`, or a package and a directory
        in it, like `fileops:go`, or a package whose app is in the package's own directory, like
        `app_go:`.

    Returns:
      The labels of the app's builds, one for each Go version.
    """
    package, _, directory = app.rpartition(":")
    package = APPS + "/" + package if package else APPS
    prefix = directory + "/" if directory else ""
    return ["{}:{}{}.go_test_app".format(package, prefix, minor) for minor in GO_VERSIONS]

# Maps each `platforms` value that a test can name to its `target_compatible_with` value.
_PLATFORMS = {
    "all": [],
    "linux": LINUX_ONLY,
    "macos": MACOS_ONLY,
    "unix": UNIX_ONLY,
    "windows": WINDOWS_ONLY,
}

# The interpreters that the tests run their Node and Python apps with, which Bazel provides, by the
# environment variable that passes each one's runfiles path to the tests. For Python, the tests run
# a program that prints the interpreter and its `PYTHONPATH` (see `bazel_python` in
# `mirrord/test-utils/src/lib.rs`).
_NODE = {
    "MIRRORD_TEST_NODE": "$(rlocationpath //mirrord/test-utils:node)",
    # Node would resolve the apps' imports, like `express`, from the real path of the app, in the
    # source tree. With the app's symlink preserved, it resolves them from the runfiles, where
    # `//:node_modules` is. The packages' own imports still resolve from their real paths, in the
    # pnpm store that rules_js lays out next to them, so only the app's symlink is preserved.
    "NODE_OPTIONS": "--preserve-symlinks-main",
}

_PYTHON = {"MIRRORD_TEST_PYTHON": "$(rlocationpath //mirrord/test-utils:python_env)"}

def _layer_test(
        name,
        platforms,
        apps = [],
        configs = [],
        scripts = [],
        node = False,
        node_modules = False,
        python = False):
    """Declares the layer test in `tests/<name>.rs`, which is its own crate, as it is for Cargo.

    The test gets the CLI's runfiles path in `MIRRORD_TESTS_USE_BINARY`. It doesn't set
    `MIRRORD_LAYER_FILE`, so the CLI loads the layer from its
    runfiles (see `mirrord/cli/src/bundle.rs`). Each test gets only the apps, configs and scripts
    that it uses, so that changing one runs only those tests again.

    Args:
      name: The test's file name, without `.rs`.
      platforms: The platforms that the file's `#![cfg]` allows: `all`, `linux`, `macos`,
        `unix` or `windows`.
      apps: The compiled apps that the test runs.
      configs: The files in `tests/configs` that the test reads.
      scripts: The scripts in `tests/apps` that the test runs with an interpreter.
      node: Whether the test runs Node apps.
      node_modules: Whether the test runs Node apps that import the packages of `//:node_modules`.
      python: Whether the test runs Python apps.
    """
    node = node or node_modules
    rust_test(
        name = name,
        # Some of the tests take about half a minute, because each one starts the CLI and an app.
        # A small test's time limit is one minute, which a busy machine can exceed.
        size = "medium",
        # Only `tests/common` holds modules that the tests share, while `rust_test`'s default would
        # take every directory of `tests/`, including the apps' sources in `tests/apps`.
        srcs = ["tests/{}.rs".format(name)] + native.glob(["tests/common/**/*.rs"]),
        data = apps + ["tests/configs/" + config for config in configs] +
               [APPS + ":" + script for script in scripts] +
               ["//mirrord/cli"] + (["//mirrord/test-utils:node"] if node else []) +
               (["//:node_modules"] if node_modules else []) +
               (["//mirrord/test-utils:python_env"] if python else []),
        env = {"MIRRORD_TESTS_USE_BINARY": "$(rlocationpath //mirrord/cli)"} |
              (_NODE if node else {}) |
              (_PYTHON if python else {}),
        target_compatible_with = _PLATFORMS[platforms],
        deps = [":layer-tests"],
    )

# The macro has no `name`, because it declares one test for each entry, named after the entry. A
# BUILD file can't pass `**test` to `_layer_test` itself, so the loop is here.
# buildifier: disable=unnamed-macro
def layer_tests(tests):
    """Declares a test for each entry in `tests`.

    Args:
      tests: The tests, by their names, each as a dict of the attributes of `_layer_test`.
    """
    for test_name, test in tests.items():
        _layer_test(name = test_name, **test)
