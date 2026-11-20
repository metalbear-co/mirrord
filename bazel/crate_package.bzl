"""The Rust macros that a workspace's crates use in place of rules_rs's rules.

rules_rust doesn't read `Cargo.toml`, so each target would have to repeat the crate name, edition,
version, features and lints that Cargo takes from it. These macros take them from the data that
rules_rs generates for each workspace member in the workspace's crate hub, like `@crates`, and from
the workspace's `@cargo_packages`, so that a crate builds with the same settings as under Cargo.

The macros are for the workspace of any module, so this file can't load mirrord's `@cargo_packages`.
Each module binds its own packages in a `crate_package` of its own, as `rust.bzl` does for mirrord.

The macros read two files of the repository whose BUILD file calls them, so the root package of
that repository must have them and make them public:

- `//:.config/nextest.toml`, nextest's configuration;
- `//:clippy.toml`, Clippy's configuration for the packages that don't have their own.

A macro can't load a file whose label it computes, so each BUILD file loads its member's data and
passes it to that `crate_package`, which returns the macros bound to it:

    load("@crates//mirrord-protocol:data.bzl", "crate_data")
    load("//bazel:rust.bzl", "crate_package")

    crate = crate_package(crate_data = crate_data())

    crate.library()

    crate.unit_tests()

They also fail the build on warnings, give each crate a `<name>_doc` target for `--config=doc`, and
run the tests with cargo-nextest.
"""

load("@rules_rs//rs:rust_binary.bzl", _rust_binary = "rust_binary")
load("@rules_rs//rs:rust_library.bzl", _rust_library = "rust_library")
load("@rules_rs//rs:rust_proc_macro.bzl", _rust_proc_macro = "rust_proc_macro")
load("@rules_rs//rs:rust_shared_library.bzl", _rust_shared_library = "rust_shared_library")

# Each member's `data.bzl` has its own `all_crate_deps`, which only calls this function with the
# member's data. Calling this function directly lets a BUILD file pass `crate_package` the data alone,
# although rules_rs keeps the function private, so a rules_rs upgrade can move it.
load("@rules_rs//rs/private:all_crate_deps.bzl", "all_crate_deps")  # buildifier: disable=bzl-visibility
load("@rules_rust//rust:defs.bzl", "rust_doc", "rust_doc_test")
load(":nextest.bzl", "rust_nextest_test")

# A string label in these macros names a target of the repository whose BUILD file calls them, so
# the targets of this repository and of its dependencies are `Label`s.

RUSTC_FLAGS = ["-Dwarnings"] + select({
    # MSVC builds link with `lld-link`, which rejects the flag below.
    Label("@llvm//constraints/windows/abi:msvc"): [],
    # rustc links through clang with `-nodefaultlibs`, so clang warns that the llvm toolchain's
    # `--unwindlib=none` (Linux) and `-rtlib=compiler-rt` (macOS) are unused, and `-Dwarnings`
    # turns that warning into an error. `module.bazelrc` turns the warning off for the tools too.
    #
    # Linux won't need the flag once rules_rs ships
    # https://github.com/hermeticbuild/rules_rust/pull/38.
    "//conditions:default": ["-Clink-arg=-Wno-unused-command-line-argument"],
})

# The symbol table is about a quarter of the Linux CLI's size, so `-c opt` builds strip it from the
# binaries and shared libraries. The toolchain's `strip_level` only strips the debug info in `opt`,
# and rules_rs's toolchains don't take another one. The dynamic symbols, which `dlsym` and the
# layer's exports need, stay.
STRIP_SYMBOLS_FLAGS = select({
    Label("//bazel:opt"): ["-Cstrip=symbols"],
    "//conditions:default": [],
})

def _crate_features(crate_data):
    """Returns the package's features, unified across the workspace as Cargo unifies them."""
    features = crate_data["crate_features"]
    features_by_platform = crate_data["crate_features_by_platform"]
    if not features_by_platform:
        return features
    return features + select(features_by_platform | {"//conditions:default": []})

def _package(crate_data, packages):
    """Returns the package's `@cargo_packages` metadata, and checks that `crate_data` is its own."""
    package = native.package_name()
    if package not in packages:
        fail(("{} isn't listed in `members` of the root `Cargo.toml`, so `@cargo_packages` has " +
              "no metadata for it. Cargo also takes path dependencies as members, but " +
              "`cargo_packages.bzl` only reads `members`.").format(package))

    package_name = packages[package].rustc_env["CARGO_PKG_NAME"]
    if crate_data["package_name"] != package_name:
        fail(("{} is the package `{}`, but its BUILD file passes the data of `{}`. It should " +
              "load `crate_data` from the `data.bzl` of `{}` in the workspace's crate hub.").format(
            package,
            package_name,
            crate_data["package_name"],
            package_name,
        ))
    return packages[package]

def _library_srcs():
    return native.glob(
        ["src/**/*.rs"],
        exclude = [
            "src/main.rs",
            "src/bin/**",
        ],
    )

def _main_binary(crate_data):
    """Returns the Cargo name of the package's main binary, or `None` when it has none.

    The main binary is the one whose file is `src/main.rs`, or the package's only binary.
    """
    binaries = crate_data["package"].targets["bin"]
    for binary, path in binaries.items():
        if path == "src/main.rs":
            return binary
    if len(binaries) == 1:
        return binaries.keys()[0]
    return None

def _binary_srcs(path, has_library):
    """Returns the sources of the binary whose crate root is `path`, as Cargo finds its modules.

    A binary in `src/bin/<name>/main.rs` has the files of its directory, and one in
    `src/bin/<name>.rs` has its file alone. A binary elsewhere, like `src/main.rs`, shares `src/`
    with the library when the package has none, and has its file alone otherwise.
    """
    if path.startswith("src/bin/"):
        if path.endswith("/main.rs"):
            return native.glob([path.removesuffix("main.rs") + "**/*.rs"])
        return [path]
    if has_library:
        return [path]
    return native.glob(["src/**/*.rs"], exclude = ["src/bin/**"])

def _clippy_config():
    """Returns the `clippy.toml` for the package's crates: the package's own, or the workspace's.

    Clippy reads only the nearest `clippy.toml`, and rules_rust would give every crate an empty one.
    """
    if native.glob(["clippy.toml"], allow_empty = True):
        return ":clippy.toml"
    return "//:clippy.toml"

def _without_optional_dependencies(crate_data, cargo_features):
    """Returns `crate_data` without the optional dependencies that `cargo_features` don't turn on.

    The `@crates` hub gives a package the optional dependencies that the workspace's features turn
    on, so a crate built with fewer features would get the others too, and code behind those
    features could use them where a build with only those features fails.
    """
    package = crate_data["package"]

    # Cargo defines a feature named after each optional dependency that `[features]` never writes as
    # `dep:<name>`, and that feature turns the dependency on.
    enabled = list(cargo_features)
    for feature in cargo_features:
        for entry in package.features.get(feature, []):
            # `<name>/<feature>` turns on the dependency, and `<name>?/<feature>` doesn't.
            name, slash, _ = entry.removeprefix("dep:").partition("/")
            if entry.startswith("dep:") or (slash and not name.endswith("?")):
                enabled.append(name)
    disabled = [
        crate
        for name, crate in package.optional_dependencies.items()
        if name not in enabled
    ]

    # A path dependency's label is its crate, like `//mirrord/kube`, and another dependency's is its
    # package's name and version, like `@crates//tokio-1.54.1`.
    def is_enabled(label):
        _, _, name_and_version = label.partition("//")
        for crate in disabled:
            if label == crate or (
                name_and_version.startswith(crate + "-") and
                name_and_version[len(crate) + 1].isdigit()
            ):
                return False
        return True

    return crate_data | {
        "deps": [label for label in crate_data["deps"] if is_enabled(label)],
        "deps_by_platform": {
            platform: [label for label in labels if is_enabled(label)]
            for platform, labels in crate_data["deps_by_platform"].items()
        },
    }

def _cargo_attrs(crate_data, kwargs, test = False):
    """Returns `kwargs` with the attributes that the package's `Cargo.toml` decides.

    An attribute in `kwargs` replaces the one from `Cargo.toml`, except `rustc_flags`,
    `crate_features`, `deps` and `rustc_env`, which `kwargs` adds to.

    `cargo_features` replaces the workspace's unified features, and leaves out the optional
    dependencies that the listed features don't turn on. It has to list the features that a listed
    one turns on too, because only the listed features' own entries in `[features]` count.

    The crate's dependencies still have the features that the workspace's build gives them, because
    the hub builds each crate once, so a crate built with `cargo_features` can use a dependency's
    feature that only the other features turn on.
    """
    kwargs = dict(kwargs)
    cargo_features = kwargs.pop("cargo_features", None)
    if cargo_features != None:
        crate_data = _without_optional_dependencies(crate_data, cargo_features)

    attrs = {
        "aliases": crate_data["aliases"],
        "rustc_flags": RUSTC_FLAGS + kwargs.pop("rustc_flags", []),
        "crate_features": (
            _crate_features(crate_data) if cargo_features == None else cargo_features
        ) + kwargs.pop("crate_features", []),
        "edition": crate_data["edition"],
        "rustc_env": crate_data["package"].rustc_env | kwargs.pop("rustc_env", {}),
        "clippy_config": _clippy_config(),
        "lint_config": crate_data.get("lint_config"),
        "version": crate_data["package"].version,
    }

    if not test:
        attrs["deps"] = all_crate_deps(
            crate_data,
            platforms = crate_data["platforms"],
            normal = True,
        ) + kwargs.pop("deps", [])
        attrs["crate_name"] = crate_data["crate_name"]

    return attrs | kwargs

def _rust_doc(crate_data, name, crate = None, **kwargs):
    rust_doc(
        name = name + "_doc",
        crate = crate or ":" + name,
        # rustdoc doesn't take the features from `crate`.
        crate_features = _crate_features(crate_data) + kwargs.get("crate_features", []),
        # So that broken links in private items fail the build too.
        rustdoc_flags = [
            "-Dwarnings",
            "--document-private-items",
        ],
        # `module.bazelrc` filters out this tag without `--config=doc`. A `--build_tag_filters` on the
        # command line replaces that filter, so it should add `-rustdoc` itself.
        tags = ["rustdoc"],
        target_compatible_with = kwargs.get("target_compatible_with"),
        visibility = ["//visibility:private"],
    )

def _rust_doc_test(crate_data, name, **kwargs):
    """Runs the doctests of the library `name`, as `cargo test` does.

    Like Cargo, it runs none when the package's `[lib]` sets `doctest = false`. rules_rust runs the doctests in a build action, and the test only reports that action's result,
    so a build of `//...` runs them too.
    """
    if not crate_data["package"].doctest:
        return

    rust_doc_test(
        name = name + "_doc_test",
        crate = ":" + name,
        # rustdoc doesn't take the features from `crate`.
        crate_features = _crate_features(crate_data) + kwargs.get("crate_features", []),
        # Doctests can use the dev-dependencies, as tests do. rules_rust's macro sorts the proc
        # macros out of them.
        deps = all_crate_deps(crate_data, platforms = crate_data["platforms"], normal_dev = True),
        # rules_rust's default, `medium`, gives the tests a 5-minute timeout, and then warns that they
        # finish far sooner. The doctests ran in the build action already.
        size = "small",
        target_compatible_with = kwargs.get("target_compatible_with"),
    )

def _binary(crate_data, name, main_crate, has_library, doc = None, **kwargs):
    """A binary crate, with the sources, the crate name and the library that Cargo gives it.

    Args:
      crate_data: The package's data, as for `crate_package`.
      name: The target's name. When it's the name of one of the package's Cargo binaries, the
        target builds that binary. Any other name builds the package's main binary, so that a
        second build of it, like the CLI's `cli_standalone`, keeps Cargo's sources and crate name
        without spelling them out. A package with several binaries and none in `src/main.rs` has no
        main binary, so another name fails there rather than guess which binary it means.
      main_crate: The name of the package's main crate, as for `library`.
      has_library: Whether the package has a library, which the binary then depends on.
      doc: Whether to document the crate. By default, only a package without a library documents
        its binaries, because a library's docs hold the package's API, and `cargo doc` skips a
        binary that has the library's name.
      **kwargs: The attributes of rules_rust's `rust_binary`.

    Returns:
      The Cargo name of the binary that the target builds, or `None` when the package has no Cargo
      binary.
    """
    binaries = crate_data["package"].targets["bin"]
    binary = name if name in binaries else _main_binary(crate_data)
    if binary == None and binaries:
        fail(("{} isn't a binary of the package `{}`, and the package has no main binary in " +
              "`src/main.rs` that a target of another name would build. Its binaries are: {}.").format(
            name,
            crate_data["package_name"],
            ", ".join(binaries.keys()),
        ))

    if binary:
        # Cargo names a binary's crate and its file after the binary rather than after the package.
        kwargs.setdefault("crate_name", binary.replace("-", "_"))
        if binary != name:
            kwargs.setdefault("binary_name", binary)
        kwargs.setdefault("crate_root", binaries[binary])
    kwargs.setdefault("srcs", _binary_srcs(binaries.get(binary, "src/main.rs"), has_library))

    if has_library:
        kwargs["deps"] = [":" + main_crate] + kwargs.get("deps", [])
    if doc == None:
        doc = not has_library

    kwargs["rustc_flags"] = kwargs.get("rustc_flags", []) + STRIP_SYMBOLS_FLAGS
    _rust_binary(name = name, **_cargo_attrs(crate_data, kwargs))
    if doc:
        _rust_doc(crate_data, name, **kwargs)
    return binary

def _library(crate_data, name, doc = True, **kwargs):
    """A library crate, with Cargo's default sources, and with `doc`, its docs and its doctests.

    The docs are `<name>_doc`, and the doctests `<name>_doc_test`.

    Args:
      crate_data: The package's data, as for `crate_package`.
      name: The target's name. A package's main crate must be named after the last part of its
        directory, like `env` for `//mirrord/agent/env`, which `crate_package` makes the default,
        because the crate data of the crates that depend on it lists it as `//mirrord/agent/env`,
        which Bazel reads as `//mirrord/agent/env:env`.
      doc: Whether to document the crate and run its doctests. A second build of the same library,
        with other features, should leave them to the first one.
      **kwargs: The attributes of rules_rust's `rust_library`.
    """
    kwargs.setdefault("srcs", _library_srcs())
    _rust_library(name = name, **_cargo_attrs(crate_data, kwargs))
    if doc:
        _rust_doc(crate_data, name, **kwargs)
        _rust_doc_test(crate_data, name, **kwargs)

def _proc_macro(crate_data, name, **kwargs):
    """A proc-macro crate, with Cargo's default sources, its `<name>_doc` target and its doctests.

    Args:
      crate_data: The package's data, as for `crate_package`.
      name: The target's name, as for `library`.
      **kwargs: The attributes of rules_rust's `rust_proc_macro`.
    """
    kwargs.setdefault("srcs", _library_srcs())
    _rust_proc_macro(name = name, **_cargo_attrs(crate_data, kwargs))
    _rust_doc(crate_data, name, **kwargs)
    _rust_doc_test(crate_data, name, **kwargs)

def _shared_library(crate_data, name, **kwargs):
    """A shared library crate, like a `cdylib`, with Cargo's default sources, and its doc target.

    Args:
      crate_data: The package's data, as for `crate_package`.
      name: The target's name, as for `library`.
      **kwargs: The attributes of rules_rust's `rust_shared_library`.
    """
    kwargs.setdefault("srcs", _library_srcs())
    attrs = _cargo_attrs(crate_data, kwargs)
    _rust_shared_library(name = name, **(attrs | {"rustc_flags": attrs["rustc_flags"] + STRIP_SYMBOLS_FLAGS}))

    # `rust_doc` doesn't take a shared library, so the docs come from a library of the same sources.
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
    _rust_doc(crate_data, name, crate = ":" + name + "_doc_crate", **kwargs)

def _test(crate_data, name, crate, binary = None, args = [], tags = [], test_threads = 1, **kwargs):
    """A test crate: a Cargo test target, or with `crate`, that crate's own `#[test]` functions.

    The tests run with cargo-nextest.

    A test of `crate` gets the package's features, but not the ones that the crate's target adds,
    so it should pass the same `crate_features`.

    Args:
      crate_data: The package's data, as for `crate_package`.
      name: The test's name.
      crate: The crate whose own tests the target runs, or `None` for the Cargo test target `name`,
        whose file is `crate_root`, `tests/<name>.rs` by default.
      binary: The Cargo name of the binary that `crate` builds, or `None` when `crate` is the
        package's library. nextest names the tests by it, like `backoffice::bin/alerts`.
      args: Arguments for `cargo nextest run`.
      tags: The test's tags. The macro adds `cpu:<test_threads>`, so that Bazel reserves that
        many CPUs for the test.
      test_threads: How many tests nextest runs at a time, unless the profile sets it.
      **kwargs: The attributes of rules_rust's `rust_test`.
    """
    if "flaky" in kwargs:
        fail("`flaky` isn't supported: nextest already retries the tests that fail (`retries` in " +
             "`.config/nextest.toml`), and `flaky` would make Bazel run the whole target again")

    # Bazel's default, `medium`, gives the tests a 5-minute timeout, and then warns that they finish
    # far sooner.
    kwargs.setdefault("size", "small")
    if not crate:
        # The test's own file, and the modules that the package's tests share, like
        # `tests/common/mod.rs`.
        crate_root = kwargs.setdefault("crate_root", "tests/{}.rs".format(name))
        kwargs.setdefault("srcs", [crate_root] + [
            src
            for src in native.glob(["tests/*/**/*.rs"], allow_empty = True)
            if src != crate_root
        ])

    package_name = crate_data["package_name"]
    if not crate:
        kind = "test"
        binary_name = name
        binary_id = package_name + "::" + name
    elif binary:
        kind = "bin"
        binary_name = binary
        binary_id = package_name + "::bin/" + binary
    else:
        kind = "lib"
        binary_name = crate_data["crate_name"]
        binary_id = package_name

    # A test of `crate` takes the crate's own dependencies from it, so it only adds the
    # dev-dependencies. The rule takes the proc macros from `proc_macro_deps` and the other crates
    # from `deps`, so both get the full list, as in rules_rust's `rust_test` macro.
    deps = all_crate_deps(
        crate_data,
        platforms = crate_data["platforms"],
        normal = not crate,
        normal_dev = True,
    ) + kwargs.pop("deps", []) + kwargs.pop("proc_macro_deps", [])

    kwargs = kwargs | {
        "args": args,
        "crate": crate,
        "deps": deps,
        "proc_macro_deps": deps,
        "nextest_binary_id": binary_id,
        "nextest_binary_name": binary_name,
        "nextest_kind": kind,
        "nextest_package_id": crate_data["package"].package_id,
        "cargo_metadata": crate_data["cargo_metadata"],
        # A string label, so that Bazel resolves it in the repository of the BUILD file that calls
        # the macro.
        "nextest_config": "//:.config/nextest.toml",
        "package_manifest": ":Cargo.toml",
        "tags": tags + ["cpu:{}".format(test_threads)],
        "test_threads": test_threads,
    }

    attrs = _cargo_attrs(crate_data, kwargs, test = True)
    if crate:
        # The test takes these from `crate`, and rules_rust adds the test's own `rustc_env` to them.
        for inherited in ["aliases", "edition"]:
            attrs.pop(inherited)
        attrs["rustc_env"] = kwargs.get("rustc_env", {})
    else:
        attrs["crate_name"] = name.replace("-", "_")

    rust_nextest_test(name = name, **attrs)

# buildifier takes `crate_package` for a macro, because the functions that it returns call rules,
# but it only returns them, so it doesn't need a `name`.
def crate_package(crate_data, packages, cargo_metadata):  # buildifier: disable=unnamed-macro
    """Returns the macros for the Rust targets of the calling BUILD file's Cargo package.

    The package's main crate is the target named after the last part of its directory, like `env`
    for `//mirrord/agent/env`, as `library` explains, so the macros name their targets that way by
    default.

    Args:
      crate_data: What `crate_data()` returns in `@crates//<name>:data.bzl`, where `<name>` is the
        `name` that the package's `Cargo.toml` gives it.
      packages: `PACKAGES` of the workspace's `@cargo_packages`.
      cargo_metadata: The `metadata.json` of the workspace's `@cargo_packages`, which the tests
        read.

    Returns:
      A struct with the macros `binary`, `library`, `proc_macro` and `shared_library`, which take
      the attributes of rules_rust's rule of the same name, like `rust_binary`, and the macros
      `unit_tests` and `integration_tests`, which take those of its `rust_test`:

      - `binary` builds the Cargo binary that its `name` names, or else the package's main binary,
        the one in `src/main.rs` or the package's only one. The name defaults to the main binary's
        in a package with a library, whose target already has the default name, and to the
        package's default name otherwise.
      - `library` and `proc_macro` are public by default, because other packages depend on them.
      - `unit_tests` runs the `#[test]` functions of the main crate, and is named `unit_tests` by
        default. It also declares `<binary>_unit_tests` for each Cargo binary that `binary`
        declares, whether before or after it, unless the binary's `[[bin]]` sets `test = false`. In
        a package without a library, the main binary is the main crate, whose tests `unit_tests`
        already runs. A binary's test takes the binary's features and platforms, and the arguments
        of `unit_tests` only go to the main crate's test. A package whose BUILD file doesn't call
        `unit_tests` runs none of these tests.
      - `integration_tests` declares a test for each of the package's Cargo test targets, like
        `tests/<name>.rs`, named after the target. As under Cargo, each test depends on the
        package's library, if the package has one, and has the package's binaries in its `data`,
        so the BUILD file declares them first. Its `tests` maps a test's name to the attributes
        that only that test takes, which replace the ones that the macro's other arguments give
        every test, except `data` and `deps`, which they add to.
    """

    # Fails here, rather than at the first target, when `crate_data` is another package's. The
    # macros read the package's metadata from `crate_data` too.
    crate_data = crate_data | {
        "cargo_metadata": cargo_metadata,
        "package": _package(crate_data, packages),
    }

    main_crate = native.package_name().split("/")[-1]
    has_library = bool(crate_data["package"].targets["lib"])

    # The binaries that `binary` declared, each with its target's name and its test's attributes,
    # and the Cargo binaries whose tests `unit_tests` declared. A BUILD file can call `binary` and
    # `unit_tests` in either order, so whichever of the two comes second declares a binary's test.
    binary_targets = []
    tested_binaries = []
    unit_tests_declared = []
    integration_tests_declared = []

    def binary_unit_tests(name, binary, attrs):
        # A second build of a binary, like the CLI's `cli_standalone`, has the same tests.
        if binary in tested_binaries or binary in crate_data["package"].untested_binaries:
            return
        tested_binaries.append(binary)
        _test(crate_data, binary + "_unit_tests", crate = ":" + name, binary = binary, **attrs)

    # Named functions rather than lambdas, so that the targets' `generator_function` names the
    # macro.
    def binary(name = None, **kwargs):
        if integration_tests_declared:
            fail("Declare the package's binaries before `integration_tests`, which gives each test " +
                 "the binaries, as Cargo does.")
        if name == None:
            # A package with a library already has a target named `main_crate`.
            name = (has_library and _main_binary(crate_data)) or main_crate
        cargo_binary = _binary(crate_data, name, main_crate, has_library, **kwargs)
        if cargo_binary == None:
            return

        # The test takes the crate's sources and dependencies, but not its features.
        attrs = {
            attr: kwargs[attr]
            for attr in ["cargo_features", "crate_features", "target_compatible_with"]
            if attr in kwargs
        }
        binary_targets.append((name, cargo_binary, attrs))
        if unit_tests_declared:
            binary_unit_tests(name, cargo_binary, attrs)

    def library(name = main_crate, visibility = ["//visibility:public"], **kwargs):
        _library(crate_data, name, visibility = visibility, **kwargs)

    def proc_macro(name = main_crate, visibility = ["//visibility:public"], **kwargs):
        _proc_macro(crate_data, name, visibility = visibility, **kwargs)

    def shared_library(name = main_crate, **kwargs):
        _shared_library(crate_data, name, **kwargs)

    def unit_tests(name = "unit_tests", **kwargs):
        # Without a library, the main crate is the main binary, if the package has one.
        main_binary = None if has_library else _main_binary(crate_data)
        if has_library or main_binary:
            _test(crate_data, name, crate = ":" + main_crate, binary = main_binary, **kwargs)
        if main_binary:
            tested_binaries.append(main_binary)

        unit_tests_declared.append(name)
        for target_name, binary, attrs in binary_targets:
            binary_unit_tests(target_name, binary, attrs)

    # One macro for every test, rather than one call per test, because Bazel doesn't find the files
    # in `tests/` as Cargo does, and a test that no BUILD file declares would never run.
    def integration_tests(tests = {}, **kwargs):
        cargo_tests = crate_data["package"].targets["test"]
        unknown_tests = [name for name in tests if name not in cargo_tests]
        if unknown_tests:
            fail("`tests` names {}, which the package `{}` has no Cargo test target for. Its tests are: {}.".format(
                ", ".join(unknown_tests),
                crate_data["package_name"],
                ", ".join(cargo_tests) or "none",
            ))

        # Cargo builds the package's binaries for its integration tests, which find them with
        # `CARGO_BIN_EXE_<name>`. A binary that two targets build, like the CLI, comes from the
        # first.
        binaries = {}
        for target_name, cargo_binary, _ in binary_targets:
            binaries.setdefault(cargo_binary, ":" + target_name)

        integration_tests_declared.append(True)
        for name, path in cargo_tests.items():
            test = tests.get(name, {})
            attrs = kwargs | test | {
                "data": binaries.values() + kwargs.get("data", []) + test.get("data", []),
                "deps": ([":" + main_crate] if has_library else []) + kwargs.get("deps", []) + test.get("deps", []),
            }
            _test(crate_data, name, crate = None, crate_root = path, **attrs)

    return struct(
        binary = binary,
        library = library,
        proc_macro = proc_macro,
        shared_library = shared_library,
        unit_tests = unit_tests,
        integration_tests = integration_tests,
    )
