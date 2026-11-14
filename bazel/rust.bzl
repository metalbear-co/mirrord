"""The Rust rules, with the attributes that Cargo would take from each crate's `Cargo.toml`.

A `.bzl` file holds Starlark code that BUILD files `load()`. The public functions here are legacy
macros, which are plain functions: when Bazel reads a BUILD file that calls one, the function runs
and declares targets with rules_rs's copies of rules_rust's rules, much as a CMake function declares
targets with `add_library`. See https://bazel.build/extending/legacy-macros.

rules_rust doesn't read `Cargo.toml`, so each target has to state the crate name, edition, version,
features and lints that Cargo would take from it. These macros take them from the `@crates` hub,
which rules_rs generates from `Cargo.toml`, and from `@cargo_packages`. Every Rust target in the
workspace uses these macros instead of the rules from rules_rs, so that it builds with the same
settings as under Cargo, and its BUILD file doesn't repeat them.

The macros also add what the workspace needs on top of Cargo's settings. They fail the build on
warnings, and turn off a linker warning that the toolchain's flags cause (see `RUSTC_FLAGS`). They
also give each crate a `<name>_doc` target that runs rustdoc on it, which
`bazel build --config=doc //...` builds.
"""

load("@cargo_packages//:packages.bzl", "PACKAGES")
load("@crates//:data.bzl", "DEP_DATA")
load("@crates//:defs.bzl", "aliases", "all_crate_deps", "crate_name", "edition", "lint_config")
load("@rules_rs//rs:rust_binary.bzl", _rust_binary = "rust_binary")
load("@rules_rs//rs:rust_library.bzl", _rust_library = "rust_library")
load("@rules_rs//rs:rust_proc_macro.bzl", _rust_proc_macro = "rust_proc_macro")
load("@rules_rs//rs:rust_shared_library.bzl", _rust_shared_library = "rust_shared_library")
load("@rules_rust//rust:defs.bzl", "rust_doc")
load(":nextest.bzl", "rust_nextest_test")

# The flags that every Rust target of the workspace gets, like `build.rustflags` in
# `.cargo/config.toml`. Third-party crates and rules_rust's own Rust code don't get them.
#
# `-Dwarnings` is like `build.warnings = "deny"` in Cargo: every warning in the workspace's crates
# fails the build.
#
# `-Wno-unused-command-line-argument` turns off clang's warning about unused arguments in links.
# The llvm toolchain passes `--unwindlib=none` (Linux) or `-rtlib=compiler-rt` (macOS) to every
# link. rustc links through clang with `-nodefaultlibs`, which makes clang ignore those flags and
# warn that they're unused, and rustc turns the warning into a `linker_messages` warning. The flags
# don't change the result, so the warning can be turned off safely. Links for MSVC don't go through
# clang, because the llvm toolchain's `lld-link` links them directly, and `lld-link` doesn't know
# the flag, so builds for MSVC leave it out. On Linux and macOS, `.bazelrc` passes the flag to the
# links of the tools that run during the build, like proc macros.
#
# https://github.com/hermeticbuild/rules_rust/pull/38 stops rules_rust from passing
# `--unwindlib=none` to rustc's links. Once rules_rs ships a rules_rust with it, builds for Linux no
# longer need the flag, here or in `.bazelrc`. Builds for macOS need it for as long as
# `-rtlib=compiler-rt` reaches their links.
RUSTC_FLAGS = ["-Dwarnings"] + select({
    "@llvm//constraints/windows/abi:msvc": [],
    "//conditions:default": ["-Clink-arg=-Wno-unused-command-line-argument"],
})

# The application manifest that the workspace's programs for MSVC embed (see the file), and the
# linker flags that embed it. `lld-link` merges the file into the manifest that it would write by
# default. A target that passes the flags needs `WINDOWS_MANIFEST` in its `compile_data`.
WINDOWS_MANIFEST = select({
    "@llvm//constraints/windows/abi:msvc": ["//bazel:long_paths.manifest"],
    "//conditions:default": [],
})
WINDOWS_MANIFEST_FLAGS = select({
    "@llvm//constraints/windows/abi:msvc": [
        "-Clink-arg=/MANIFEST:EMBED",
        "-Clink-arg=/MANIFESTINPUT:$(execpath //bazel:long_paths.manifest)",
    ],
    "//conditions:default": [],
})

def _crate_features():
    """Returns the package's features, unified across the workspace as Cargo unifies them.

    rules_rs resolves them, but unlike the edition, the `@crates` hub has no function that returns
    them, so this reads them from the hub's data.
    """
    dep_data = DEP_DATA.get(native.package_name())
    if not dep_data:
        return []

    features = dep_data["crate_features"]
    features_by_platform = dep_data["crate_features_by_platform"]
    if not features_by_platform:
        return features
    return features + select(features_by_platform | {"//conditions:default": []})

def _package():
    """Returns the metadata of the package's crate from its `Cargo.toml` (see `@cargo_packages`).

    Its `version` is usually the workspace's, but some crates have their own, like
    `mirrord-protocol`, whose version is the protocol version that the agent and the CLI negotiate.
    """
    package = native.package_name()
    if package not in PACKAGES:
        fail(("{} isn't listed in `members` of the root `Cargo.toml`, so `@cargo_packages` has " +
              "no metadata for it. Cargo also takes path dependencies as members, but " +
              "`//bazel:cargo_packages.bzl` only reads `members`.").format(package))
    return PACKAGES[package]

def _library_srcs():
    """Returns the sources of the package's library, as Cargo finds them from `src/lib.rs`."""
    return native.glob(
        ["src/**/*.rs"],
        exclude = [
            "src/main.rs",
            "src/bin/**",
        ],
    )

def _binary_srcs():
    """Returns the sources of the package's binary, as Cargo finds them from `src/main.rs`.

    A binary of a package that also has a library only takes `src/main.rs`, and uses the library's
    modules through the library.
    """
    if native.glob(["src/lib.rs"], allow_empty = True):
        return ["src/main.rs"]
    return native.glob(["src/**/*.rs"], exclude = ["src/bin/**"])

def _binary_name():
    """Returns the name of the package's binary: its `[[bin]]` name, or the package's name.

    Returns `None` unless the package has exactly one binary.
    """
    binaries = DEP_DATA.get(native.package_name(), {}).get("binaries", {})
    if len(binaries) == 1:
        return binaries.keys()[0]
    return None

def _clippy_config():
    """Returns the `clippy.toml` that clippy reads for the package's crates.

    Clippy reads only the nearest `clippy.toml`, so a package with its own file doesn't get the
    workspace's. rules_rust would otherwise give every crate an empty one.
    """
    if native.glob(["clippy.toml"], allow_empty = True):
        return ":clippy.toml"
    return "//:clippy.toml"

def _cargo_attrs(kwargs, test = False):
    """Returns `kwargs` with the attributes that the package's `Cargo.toml` decides.

    An attribute in `kwargs` overrides the one from `Cargo.toml`, except `rustc_flags`,
    `crate_features` and `deps`, which are added to the ones here, and `rustc_env`, whose variables
    are merged with the ones here. A `test` passes its full `deps` and its crate's name itself (see
    `rust_test`).

    `cargo_features` in `kwargs` replaces the package's features that the workspace unifies, so the
    crate's own `cfg(feature = "...")`s are those of
    `cargo build -p <package> --no-default-features --features <features>`. Nothing here reads the
    `[features]` table, so the list must also name the features that a listed feature turns on. The
    dependencies stay the workspace's, because rules_rs resolves one set of them for each package:
    the target gets every optional dependency that the workspace's features turn on, and each
    dependency has the features that the workspace gives it.
    """
    kwargs = dict(kwargs)
    cargo_features = kwargs.pop("cargo_features", None)

    attrs = {
        "aliases": aliases(),
        "rustc_flags": RUSTC_FLAGS + kwargs.pop("rustc_flags", []),
        "crate_features": (
            _crate_features() if cargo_features == None else cargo_features
        ) + kwargs.pop("crate_features", []),
        "edition": edition(),
        # rules_rust would set `CARGO_PKG_NAME` to the target's name, and leave the description and
        # the other fields empty.
        "rustc_env": _package().rustc_env | kwargs.pop("rustc_env", {}),
        "clippy_config": _clippy_config(),
        "lint_config": lint_config(),
        "version": _package().version,
    }

    if not test:
        # The package's dependencies from `Cargo.toml`, and the targets of the workspace that
        # `deps` adds, like the library of the package's own binary.
        attrs["deps"] = all_crate_deps(normal = True) + kwargs.pop("deps", [])
        attrs["crate_name"] = crate_name()

    return attrs | kwargs

def _cargo_files():
    """Declares the package's `cargo_files`, unless an earlier macro in the package did.

    `cargo_files` holds the files that `cargo metadata` reads to describe the package: its
    `Cargo.toml` and the sources of the targets that Cargo discovers. `//:cargo_metadata` takes
    them from every package, for nextest (see `//bazel:nextest.bzl`).
    """
    if native.existing_rule("cargo_files"):
        return
    native.filegroup(
        name = "cargo_files",
        srcs = native.glob(
            [
                "Cargo.toml",
                "src/lib.rs",
                "src/main.rs",
                "src/bin/*.rs",
                "src/bin/*/main.rs",
                "tests/*.rs",
                "tests/*/main.rs",
                "examples/*.rs",
                "examples/*/main.rs",
                "benches/*.rs",
                "benches/*/main.rs",
            ],
            allow_empty = True,
        ),
        visibility = ["//:__pkg__"],
    )

def _rust_doc(name, crate = None, **kwargs):
    rust_doc(
        name = name + "_doc",
        crate = crate or ":" + name,
        # rustdoc doesn't take the crate's features from the crate, so it would leave out the items
        # behind them.
        crate_features = _crate_features() + kwargs.get("crate_features", []),
        # The docs cover the private items too, as `cargo doc --document-private-items` would, so that
        # their broken links fail the build as well.
        rustdoc_flags = [
            "-Dwarnings",
            "--document-private-items",
        ],
        # `.bazelrc` leaves out targets with this tag, unless `--config=doc` selects only them. So a
        # command that names a doc target needs `--config=doc` too, and a `--build_tag_filters` on
        # the command line replaces `.bazelrc`'s, so it should add `-rustdoc` itself.
        tags = ["rustdoc"],
        target_compatible_with = kwargs.get("target_compatible_with"),
        visibility = ["//visibility:private"],
    )

def rust_binary(name, doc = True, **kwargs):
    """A binary crate, and with `doc`, its `<name>_doc` target.

    Its sources and its binary's name default to Cargo's: `src/main.rs` and the modules that it
    finds, and the binary's name from `Cargo.toml`, like `mirrord` for `//mirrord/cli`. A binary for
    MSVC embeds the application manifest `WINDOWS_MANIFEST`.

    Args:
      name: The target's name. The main crate of a package that other crates depend on must be
        named after the last part of the package's directory, like `env` for
        `//mirrord/agent/env`, because rules_rs's `all_crate_deps` names it that way.
      doc: Whether to document the crate. A binary of a package that also has a library leaves
        it to the library, as Cargo does.
      **kwargs: The attributes of rules_rust's `rust_binary`.
    """
    _cargo_files()

    binary_name = _binary_name()
    if binary_name:
        # Cargo names a binary's crate after the binary rather than after the package, so
        # `mirrord-cli`'s binary is the crate `mirrord`, as its logs' targets show.
        kwargs.setdefault("crate_name", binary_name.replace("-", "_"))

    if binary_name and binary_name != name:
        kwargs.setdefault("binary_name", binary_name)

    kwargs.setdefault("srcs", _binary_srcs())
    kwargs = kwargs | {
        "compile_data": kwargs.get("compile_data", []) + WINDOWS_MANIFEST,
        "rustc_flags": kwargs.get("rustc_flags", []) + WINDOWS_MANIFEST_FLAGS,
    }

    _rust_binary(name = name, **_cargo_attrs(kwargs))
    if doc:
        _rust_doc(name, **kwargs)

def rust_library(name, doc = True, **kwargs):
    """A library crate, and with `doc`, its `<name>_doc` target.

    Its sources default to Cargo's: `src/lib.rs` and the modules that it finds.

    Args:
      name: The target's name. The main crate of a package that other crates depend on must be
        named after the last part of the package's directory, like `env` for
        `//mirrord/agent/env`, because rules_rs's `all_crate_deps` names it that way.
      doc: Whether to document the crate. A second build of the same library, with other
        features, leaves it to the first one.
      **kwargs: The attributes of rules_rust's `rust_library`.
    """
    _cargo_files()
    kwargs.setdefault("srcs", _library_srcs())
    _rust_library(name = name, **_cargo_attrs(kwargs))
    if doc:
        _rust_doc(name, **kwargs)

def rust_proc_macro(name, **kwargs):
    """A proc-macro crate, and its `<name>_doc` target.

    Its sources default to Cargo's, as for `rust_library`.

    Args:
      name: The target's name, as for `rust_library`.
      **kwargs: The attributes of rules_rust's `rust_proc_macro`.
    """
    _cargo_files()
    kwargs.setdefault("srcs", _library_srcs())
    _rust_proc_macro(name = name, **_cargo_attrs(kwargs))
    _rust_doc(name, **kwargs)

def rust_shared_library(name, **kwargs):
    """A shared library crate, like a `cdylib`, and its `<name>_doc` target.

    Its sources default to Cargo's, as for `rust_library`.

    Args:
      name: The target's name, as for `rust_library`.
      **kwargs: The attributes of rules_rust's `rust_shared_library`.
    """
    _cargo_files()
    kwargs.setdefault("srcs", _library_srcs())
    attrs = _cargo_attrs(kwargs)
    _rust_shared_library(name = name, **attrs)

    # `rust_doc` only takes targets that provide a crate to other Rust code, which a shared library
    # doesn't. So its docs come from a library of the same sources, which only the docs build
    # compiles.
    _rust_library(
        name = name + "_doc_crate",
        tags = ["manual"],
        visibility = ["//visibility:private"],
        **{
            key: value
            for key, value in attrs.items()
            if key not in ["cc_runtime_linkage", "tags", "visibility"]
        }
    )
    _rust_doc(name, crate = ":" + name + "_doc_crate", **kwargs)

def rust_test(name, args = [], tags = [], test_threads = 1, **kwargs):
    """A test crate: a file in `tests/`, or a crate's own `#[test]` functions with `crate`.

    The crate of a file in `tests/` is named after the file, as Cargo names it. A test of a crate's
    own functions takes everything but the version and the features from that crate. It gets the
    features from `Cargo.toml`, as the crate does, but not the `crate_features` that the crate's
    target adds, so the test must pass the same `crate_features`.

    The tests run with cargo-nextest, as `cargo nextest run` runs them (see `//bazel:nextest.bzl`),
    so each test runs in a process of its own, and `.config/nextest.toml` sets the retries and the
    timeouts. So the test can't be `flaky`: Bazel would run the whole target again on top of
    nextest's retries.

    A test is small unless it says otherwise. Bazel would make it medium, with a 5-minute timeout,
    and then warn that the tests finish far faster than that.

    Args:
      name: The test's name.
      args: Arguments for `cargo nextest run`.
      tags: The test's tags. The macro adds `cpu:<test_threads>`, so that Bazel reserves that
        many CPUs for the test.
      test_threads: How many tests nextest runs at a time, unless the profile sets it.
      **kwargs: The attributes of rules_rust's `rust_test`.
    """
    if "flaky" in kwargs:
        fail("`flaky` isn't supported: nextest already retries the tests that fail (`retries` in " +
             "`.config/nextest.toml`), and `flaky` would make Bazel run the whole target again")

    _cargo_files()
    kwargs.setdefault("size", "small")
    if "crate" not in kwargs:
        # A file in `tests/` is a test crate of its own, and its directories hold modules that the
        # test files share, like `tests/common`, as Cargo treats them.
        kwargs.setdefault(
            "srcs",
            ["tests/{}.rs".format(name)] + native.glob(["tests/*/**/*.rs"], allow_empty = True),
        )

    package_data = DEP_DATA.get(native.package_name(), {})
    package_name = _package().rustc_env["CARGO_PKG_NAME"]
    if "crate" not in kwargs:
        kind = "test"
        binary_name = name
        binary_id = package_name + "::" + name
    elif package_data.get("binaries") and not native.glob(["src/lib.rs"], allow_empty = True):
        # The tests of a binary crate, in a package without a library, as in `mirrord/cli`. Its only
        # binary is the crate. In a package that has both, like `mirrord/tui`, the tests are the
        # library's.
        kind = "bin"
        binary_name = package_data["binaries"].keys()[0]
        binary_id = package_name + "::bin/" + binary_name
    else:
        kind = "lib"
        binary_name = crate_name()
        binary_id = package_name

    # The rule takes the proc macros from `proc_macro_deps`, which it builds for the machine that
    # runs the compiler, and the other crates from `deps`. So both attributes get the full list, as
    # in rules_rust's `rust_test` macro. A test of a crate's own functions takes the crate's
    # dependencies from the crate, and adds the package's dev-dependencies, as Cargo does. A file in
    # `tests/` takes both.
    deps = all_crate_deps(
        normal = "crate" not in kwargs,
        normal_dev = True,
    ) + kwargs.pop("deps", []) + kwargs.pop("proc_macro_deps", [])

    kwargs = kwargs | {
        "args": ["test"] + args,
        "deps": deps,
        "proc_macro_deps": deps,
        "nextest_binary_id": binary_id,
        "nextest_binary_name": binary_name,
        "nextest_kind": kind,
        "package_manifest": ":Cargo.toml",
        "tags": tags + ["cpu:{}".format(test_threads)],
        "test_threads": test_threads,
    }

    attrs = _cargo_attrs(kwargs, test = True)
    if "crate" in kwargs:
        # A test of a crate's own functions takes these from the crate. rules_rust adds the test's
        # own `rustc_env` to the crate's.
        for inherited in ["aliases", "edition"]:
            attrs.pop(inherited)
        attrs["rustc_env"] = kwargs.get("rustc_env", {})
    else:
        # The crate is named after the test, as Cargo names it, rather than after this target.
        attrs["crate_name"] = name.replace("-", "_")

    rust_nextest_test(name = name, **attrs)
