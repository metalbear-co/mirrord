"""`rust_nextest_test`, a Rust test that runs under cargo-nextest.

rules_rust's `rust_test` runs the libtest binary that it builds, and libtest runs all of a crate's
tests on threads of one process. mirrord's tests need nextest instead, for three reasons:

- Some tests change the process's environment or working directory, which would race with the
  other tests on libtest's threads. nextest runs each test in a process of its own.
- `.config/nextest.toml` sets the retries, the timeouts, and the profiles that pick which
  end-to-end tests run. libtest has none of these.
- nextest's JUnit report has a test case for each test, so Bazel and CI show which test failed.

nextest only runs the binaries of a Cargo build, and its maintainer declined to support Bazel
(https://github.com/nextest-rs/nextest/issues/842). In a later discussion, they said that an
integrated solution would be a lot of work, and pointed to the `--cargo-metadata` and
`--binaries-metadata` options of `cargo nextest run`, which take the output of `cargo metadata` and
the list of test binaries that a Cargo build would write
(https://github.com/nextest-rs/nextest/discussions/2421). So `cargo_packages` writes the first file
once for the workspace, and each test's executable, `//bazel/nextest-runner`, writes the second and
runs nextest.

`rust_nextest_test` extends `rust_test` (https://bazel.build/rules/rules#extend) rather than
wrapping it, so the target still builds the test binary, keeps `rust_test`'s attributes and
providers, and gets linted by clippy like any other crate. Only its executable changes.

nextest retries the failed tests itself, so a target shouldn't be `flaky`, or Bazel would run the
whole target again on top of nextest's retries.

Each `rust_nextest_test` runs its own binary in a nextest run of its own. `rust_nextest_suite` runs
the binaries of several tests in one run, so that the run's limits, like its test threads and the
test groups of `.config/nextest.toml`, apply to all of their tests together, as they do in a
`cargo nextest run` of the whole workspace. Both rules give the runner their binaries in
`NextestInfo`.
"""

load("@bazel_lib//lib:paths.bzl", "to_rlocation_path")
load("@bazel_skylib//lib:paths.bzl", "paths")
load("@rules_rust//rust:rust_common.bzl", "CrateInfo")

# Only a rule can be extended, and rules_rust's public `rust_test` is a macro around this one.
# buildifier: disable=bzl-visibility
load("@rules_rust//rust/private:rust.bzl", "rust_test")

def _find_provider(providers, kind, has = None):
    """Returns the provider in `providers` whose type is `kind`, and that has the field `has`.

    A rule extension gets its parent's providers as a list. Starlark names the type of a built-in
    provider, but calls every rules_rust provider a `struct`, so `has` tells those apart by a field.
    """
    for provider in providers:
        if type(provider) == kind and (has == None or hasattr(provider, has)):
            return provider

    if has == None:
        fail("rust_test returned no {}".format(kind))
    fail("rust_test returned no {} with `{}`".format(kind, has))

NextestInfo = provider(
    doc = "The test binaries of a nextest target, and the settings that the runner runs them with.",
    fields = {
        "binaries": "The test binaries, as a list of dicts. Each dict has the binary's `id`, " +
                    "`kind`, `name` and `package_id` in nextest's list of test binaries, and its " +
                    "`path` in the runfiles.",
        "settings": "The runner's settings that every binary of a run shares, like the " +
                    "workspace's metadata, as a dict from each setting's name without " +
                    "`BAZEL_NEXTEST_` to its value.",
    },
)

def _runner_executable(ctx):
    """Declares the target's executable, a link to the runner.

    A rule's executable must be a file that the rule creates, so the executable can't be the runner
    itself. Windows only runs a program whose name ends with `.exe`.
    """
    windows = ctx.target_platform_has_constraint(ctx.attr._nextest_windows[platform_common.ConstraintValueInfo])
    executable = ctx.actions.declare_file(ctx.label.name + "_nextest" + (".exe" if windows else ""))
    ctx.actions.symlink(
        output = executable,
        target_file = ctx.executable._nextest_runner,
        is_executable = True,
    )
    return executable

def _runner_environment(nextest_info, test_threads):
    """Returns the `BAZEL_NEXTEST_*` variables that the runner reads its settings from."""
    settings = nextest_info.settings | {
        "BINARIES": json.encode(nextest_info.binaries),
        "TEST_THREADS": str(test_threads),
    }
    return {"BAZEL_NEXTEST_" + name: value for name, value in settings.items()}

# The attributes that run the runner, which both rules have.
_RUNNER_ATTRS = {
    "_nextest_runner": attr.label(
        default = "//bazel/nextest-runner",
        cfg = "target",
        executable = True,
    ),
    "_nextest_windows": attr.label(default = "@platforms//os:windows"),
}

def _rust_nextest_test_impl(ctx):
    providers = ctx.super()
    default_info = _find_provider(providers, "DefaultInfo")
    run_environment_info = _find_provider(providers, "RunEnvironmentInfo")

    # The parent's `DefaultInfo` has no `files_to_run`, because it doesn't belong to a target yet.
    # So the test binary comes from rules_rust's `CrateInfo`, the provider with `is_test`.
    binary = _find_provider(providers, "struct", has = "is_test").output

    runner = ctx.executable._nextest_runner

    # The directory of the test's repository in the runfiles. `ctx.workspace_name` is the main
    # repository's, which is another one when another module runs a test of this module.
    workspace = ctx.label.workspace_name or ctx.workspace_name

    executable = _runner_executable(ctx)
    nextest = ctx.file._nextest

    nextest_info = NextestInfo(
        binaries = [{
            "id": ctx.attr.nextest_binary_id,
            "kind": ctx.attr.nextest_kind,
            "name": ctx.attr.nextest_binary_name,
            "package_id": ctx.attr.nextest_package_id,
            "path": to_rlocation_path(ctx, binary),
        }],
        settings = {
            "CARGO_METADATA": to_rlocation_path(ctx, ctx.file.cargo_metadata),
            "CONFIG": to_rlocation_path(ctx, ctx.file.nextest_config),
            "NEXTEST": to_rlocation_path(ctx, nextest),

            # The `platform()` filters and `cfg()` overrides in `.config/nextest.toml` match the
            # triple.
            "TRIPLE": ctx.toolchains["@rules_rust//rust:toolchain_type"].target_flag_value,

            # nextest only checks that the workspace's `Cargo.toml` exists, so the runfiles hold an
            # empty one, and a change to the real one, like a new dependency, doesn't rerun every
            # test.
            "WORKSPACE_MANIFEST": workspace + "/Cargo.toml",
        },
    )

    # For each Rust binary in `data`, rules_rust compiles `CARGO_BIN_EXE_<name>` into the test, as
    # the binary's path from the directory where Bazel starts a test, the main repository's in the
    # runfiles. nextest starts each test in its package's directory instead, as Cargo does, so a
    # link puts the binary at the same path from there, and the test finds it with
    # `env!("CARGO_BIN_EXE_<name>")`.
    #
    # A link can't go under a path that is a file of the runfiles, so the rule leaves out the link
    # of such a binary, and the test can't use its variable. For example, a test of `//mirrord/cli`
    # with the CLI, `mirrord/cli/mirrord`, in its `data` would need the link
    # `mirrord/cli/mirrord/cli/mirrord`.
    data_files = [paths.normalize(workspace + "/" + file.short_path) for file in ctx.files.data]
    data_binaries = {}
    for data in ctx.attr.data:
        if CrateInfo not in data or data[CrateInfo].type != "bin":
            continue
        output = data[CrateInfo].output
        link = paths.normalize("/".join([workspace, ctx.label.package, output.short_path]))
        if not [file for file in data_files if link.startswith(file + "/")]:
            data_binaries[link] = output

    runfiles = ctx.runfiles(
        files = [binary, ctx.file.cargo_metadata, ctx.file.nextest_config, nextest, runner] +
                ctx.files.package_manifest,
        root_symlinks = data_binaries | {
            workspace + "/Cargo.toml": ctx.file._nextest_workspace_manifest,
        },
    ).merge(default_info.default_runfiles)

    environment = run_environment_info.environment | _runner_environment(nextest_info, ctx.attr.test_threads)

    return [
        DefaultInfo(
            executable = executable,
            # So that `bazel build` of the test builds the test binary, as for a `rust_test`.
            files = depset([executable], transitive = [default_info.files]),
            runfiles = runfiles,
        ),
        RunEnvironmentInfo(
            environment = environment,
            inherited_environment = run_environment_info.inherited_environment,
        ),
        nextest_info,
    ] + [
        provider
        for provider in providers
        if provider != default_info and provider != run_environment_info
    ]

rust_nextest_test = rule(
    implementation = _rust_nextest_test_impl,
    doc = """A `rust_test` whose tests run with cargo-nextest.

    The runner finds the Cargo package by the Bazel package, so the target must be in its Cargo
    package's directory.

    rules_rust takes the proc macros from `proc_macro_deps` and the other crates from `deps`, so a
    caller must pass all of the dependencies to both.""",
    parent = rust_test,
    attrs = {
        "cargo_metadata": attr.label(
            doc = "The workspace's packages in the format of `cargo metadata`, the " +
                  "`metadata.json` of its `cargo_packages`.",
            allow_single_file = True,
            mandatory = True,
        ),
        "nextest_binary_id": attr.string(
            doc = "The binary's ID in nextest, like `mirrord-layer-tests::c_stat`, which the " +
                  "JUnit report names test suites by.",
            mandatory = True,
        ),
        "nextest_binary_name": attr.string(
            doc = "The name of the binary's Cargo target, which nextest's list of test " +
                  "binaries requires.",
            mandatory = True,
        ),
        "nextest_config": attr.label(
            doc = "nextest's configuration, with the profiles, the retries and the timeouts.",
            allow_single_file = True,
            mandatory = True,
        ),
        "nextest_kind": attr.string(
            doc = "The kind of the binary's Cargo target, which `kind()` filters match: `test` " +
                  "for a file in `tests/`, and `lib` or `bin` for a crate's own tests.",
            values = ["bin", "lib", "test"],
            mandatory = True,
        ),
        "nextest_package_id": attr.string(
            doc = "The `package_id` of the binary's Cargo package, which names the package in " +
                  "`cargo_metadata`.",
            mandatory = True,
        ),
        "package_manifest": attr.label(
            doc = "The package's `Cargo.toml`, for the tests that read it from their working " +
                  "directory, the package's directory.",
            allow_single_file = True,
        ),
        "test_threads": attr.int(
            doc = "How many tests nextest runs at a time, unless the profile sets it. " +
                  "The test macros of `crate_package` tag the target `cpu:<test_threads>`, so " +
                  "that Bazel reserves as many CPUs for it.",
            default = 1,
        ),
        "_nextest_workspace_manifest": attr.label(
            allow_single_file = True,
            default = "//bazel/nextest-runner:workspace.Cargo.toml",
        ),
        # The release archive's program is a source file, so it can't be an `executable` attribute.
        "_nextest": attr.label(
            allow_single_file = True,
            cfg = "target",
            default = "//bazel/third_party/nextest",
        ),
    } | _RUNNER_ATTRS,
)

def _rust_nextest_suite_impl(ctx):
    tests = ctx.attr.tests
    first = tests[0]
    for test in tests[1:]:
        if test[NextestInfo].settings != first[NextestInfo].settings:
            fail("{} and {} can't run in one nextest run, because they have different settings, like another workspace or target platform: {} and {}".format(
                first.label,
                test.label,
                first[NextestInfo].settings,
                test[NextestInfo].settings,
            ))

    # The runner hands nextest the binaries by their binary ID, so a second binary with the same ID,
    # like a library's unit tests built with other features, would replace the first one.
    binaries = {}
    for test in tests:
        for binary in test[NextestInfo].binaries:
            other, owner = binaries.setdefault(binary["id"], (binary, test.label))
            if other["path"] != binary["path"]:
                fail("{} and {} have the same nextest binary ID, {}, so one nextest run can't run both.".format(
                    owner,
                    test.label,
                    binary["id"],
                ))

    nextest_info = NextestInfo(
        binaries = [binary for binary, _ in binaries.values()],
        settings = first[NextestInfo].settings,
    )

    # nextest runs every binary with the runner's environment, so the run gets the variables of
    # every test, and the runner's settings of the suite rather than those of a test. rules_rust
    # gives each test its package's `CARGO_MANIFEST_DIR`, which nextest sets itself for each
    # binary.
    environment = {}
    for test in tests:
        for name, value in test[RunEnvironmentInfo].environment.items():
            if name.startswith("BAZEL_NEXTEST_") or name == "CARGO_MANIFEST_DIR":
                continue
            if environment.get(name, value) != value:
                fail("The tests give the variable {} two values, {} and {}, and nextest runs every binary with the same environment.".format(
                    name,
                    environment[name],
                    value,
                ))
            environment[name] = value

    inherited_environment = []
    for test in tests:
        for name in test[RunEnvironmentInfo].inherited_environment:
            if name not in inherited_environment:
                inherited_environment.append(name)

    return [
        DefaultInfo(
            executable = _runner_executable(ctx),
            runfiles = ctx.runfiles().merge_all([test[DefaultInfo].default_runfiles for test in tests]),
        ),
        RunEnvironmentInfo(
            environment = environment | _runner_environment(nextest_info, ctx.attr.test_threads),
            inherited_environment = inherited_environment,
        ),
        nextest_info,
    ]

_rust_nextest_suite_test = rule(
    implementation = _rust_nextest_suite_impl,
    doc = "Runs the binaries of several `rust_nextest_test` targets in one nextest run.",
    test = True,
    attrs = {
        "tests": attr.label_list(
            doc = "The `rust_nextest_test` targets, or other suites, whose binaries the run runs.",
            mandatory = True,
            allow_empty = False,
            providers = [NextestInfo, RunEnvironmentInfo],
        ),
        "test_threads": attr.int(
            doc = "How many tests nextest runs at a time, unless the profile sets it.",
            default = 1,
        ),
    } | _RUNNER_ATTRS,
)

def rust_nextest_suite(name, tests, test_threads = 1, tags = [], **kwargs):
    """Runs the test binaries of `tests` in one nextest run.

    A `rust_nextest_test` runs one binary, so a nextest run has the test threads, the test groups
    and the timeouts of `.config/nextest.toml` to itself. A suite runs the tests of all its
    binaries together, as `cargo nextest run` does for a workspace: `test_threads` and the test
    groups limit them across binaries, filters like `binary(=pg)` pick among them, and the shards of
    `shard_count` split them all. A `cluster_test` can run a suite as its `tests`.

    The binaries must be of one workspace and one target platform. nextest runs every binary with
    the same environment, so the run gets the variables of every test, and two tests mustn't give a
    variable two values, nor two binaries one nextest binary ID, like two `crate.unit_tests` of one
    library. The arguments of the tests don't apply, only the suite's `args`.

        rust_nextest_suite(
            name = "engines",
            tests = [":mysql", ":pg"],
            test_threads = 16,
        )

    Args:
      name: The suite's name.
      tests: The `rust_nextest_test` targets whose binaries the suite runs, like those of
        `crate.unit_tests` and `crate.integration_tests`, or other suites.
      test_threads: How many tests nextest runs at a time, unless the profile sets it. The macro
        tags the target `cpu:<test_threads>`, so that Bazel reserves as many CPUs for it.
      tags: The suite's tags.
      **kwargs: The attributes of a test, like `args`, `timeout` and `shard_count`.
    """
    _rust_nextest_suite_test(
        name = name,
        tests = tests,
        test_threads = test_threads,
        tags = tags + ["cpu:{}".format(test_threads)],
        **kwargs
    )
