"""`rust_nextest_test`, a test that runs a Rust test binary with cargo-nextest.

A `rust_test` on its own runs the libtest binary that it builds, which runs all of the crate's tests
on threads of one process. The workspace's tests run with cargo-nextest instead,
which runs each test in a process of its own, retries the tests that fail, and writes a JUnit
report with a test case for each test. Some tests rely on that: they change the process's
environment or working directory, which would race with the other tests on libtest's threads.

`rust_nextest_test` is rules_rust's `rust_test`, extended (https://bazel.build/rules/rules#extend)
so that its executable runs the binary with the same nextest, configured by the same
`.config/nextest.toml`, and the tests behave as they do under `cargo nextest run`. The target is
still a `rust_test` in every other way, so clippy and rust-analyzer see it as one.

nextest only runs the binaries of a Cargo workspace, which it learns about from two JSON files that
`cargo nextest run` takes in place of a build (https://nexte.st/docs/ci-features/archiving/):

- `--cargo-metadata`, the output of `cargo metadata`. nextest takes the workspace's root, each
  package's directory and its targets from it. It also checks every package and binary that a
  filter in `.config/nextest.toml` names against it, so it must describe the whole workspace,
  even though a test runs few of its binaries. `cargo_metadata` below writes it, with only the
  fields that nextest reads, so that it only changes when a package or a target is added, removed
  or renamed, or a version changes. Otherwise any change to any `Cargo.toml`, like a new
  dependency, would run every test again. The file places the workspace at a made-up root, and
  each test moves it to the test's runfiles with `--workspace-remap`.
- `--binaries-metadata`, the list of test binaries that a build would write. The test's runner
  writes it with the binary that the test runs.

The runner, `//bazel/nextest-runner`, is the test's executable. It's a Rust program, so that the
tests also run on Windows machines, and it translates Bazel's test protocol for nextest
(https://bazel.build/reference/test-encyclopedia):

- `--test_filter` runs only the tests whose names contain the filter.
- `shard_count` splits the sorted tests evenly between the shards (`--partition slice:`).
- The JUnit report that nextest writes becomes the target's `test.xml`.
- nextest ends the run before Bazel's timeout for the target does, so that it reports the tests
  that were still running (`global-timeout`).
- `test_threads` is how many tests nextest runs at a time, and Bazel reserves as many CPUs for the
  target.
- `--test_arg` passes options to `cargo nextest run`, for example `--test_arg=-E
  --test_arg='test(/^fs::/)'` or `--test_arg=--run-ignored=all`. The runner takes its own settings
  from the environment, so every argument of the test is nextest's.
- `--test_env=NEXTEST_PROFILE=ci` picks a profile of `.config/nextest.toml`, and the other
  `NEXTEST_*` variables, like `NEXTEST_RETRIES`, override its settings.

nextest retries the tests that fail (`retries` in `.config/nextest.toml`), so Bazel must not also
run the whole target again: the targets aren't `flaky`, and `--flaky_test_attempts` would multiply
the attempts. nextest also stops a run at its first failing test, as `cargo nextest run` does, while
Bazel's other test runners run every test unless `--test_runner_fail_fast` is set.
"""

load("@bazel_lib//lib:paths.bzl", "to_rlocation_path")

# rules_rust's public `rust_test` is a macro around this rule, and only a rule can be extended, so
# this loads the rule from rules_rust's private code.
# buildifier: disable=bzl-visibility
load("@rules_rust//rust/private:rust.bzl", "rust_test")

def _cargo_metadata_impl(ctx):
    cargo = ctx.toolchains["@rules_rust//cargo:toolchain_type"]
    output = ctx.actions.declare_file(ctx.label.name + ".json")

    ctx.actions.run(
        executable = ctx.executable._runner,
        arguments = [
            "metadata",
            cargo.cargo.path,
            ctx.file.workspace_manifest.path,
            output.path,
        ],
        inputs = ctx.files.srcs + [ctx.file.workspace_manifest],
        tools = cargo.all_files,
        outputs = [output],
        mnemonic = "CargoMetadata",
        progress_message = "Describing the Cargo workspace for nextest",
    )

    return [DefaultInfo(files = depset([output]))]

cargo_metadata = rule(
    implementation = _cargo_metadata_impl,
    doc = "The workspace's packages and their targets, as `cargo metadata --no-deps` writes them.",
    attrs = {
        "srcs": attr.label_list(
            doc = "Each package's `Cargo.toml` and the sources of its targets that Cargo " +
                  "discovers, like `src/lib.rs` and `tests/*.rs`.",
            allow_files = True,
        ),
        "workspace_manifest": attr.label(
            doc = "The workspace's root `Cargo.toml`.",
            allow_single_file = True,
            default = "//:Cargo.toml",
        ),
        "_runner": attr.label(
            default = "//bazel/nextest-runner",
            cfg = "exec",
            executable = True,
        ),
    },
    toolchains = ["@rules_rust//cargo:toolchain_type"],
)

def _find_provider(providers, kind, has = None):
    """Returns the provider in `providers` whose type is `kind`, and that has the field `has`.

    A rule extension gets its parent's providers as a list. Starlark names the type of a built-in
    provider, but calls every provider of rules_rust a `struct`, so those are told apart by a field.
    """
    for provider in providers:
        if type(provider) == kind and (has == None or hasattr(provider, has)):
            return provider

    if has == None:
        fail("rust_test returned no {}".format(kind))
    fail("rust_test returned no {} with `{}`".format(kind, has))

def _rust_nextest_test_impl(ctx):
    providers = ctx.super()
    default_info = _find_provider(providers, "DefaultInfo")
    run_environment_info = _find_provider(providers, "RunEnvironmentInfo")

    # The test binary. The parent's `DefaultInfo` doesn't belong to a target yet, so it has no
    # `files_to_run`, and rules_rust's `CrateInfo` names the binary instead.
    binary = _find_provider(providers, "struct", has = "is_test").output

    # The test's executable is the runner, under the test's name. Windows only runs a program whose
    # name ends with `.exe`.
    runner = ctx.executable._nextest_runner
    windows = ctx.target_platform_has_constraint(ctx.attr._nextest_windows[platform_common.ConstraintValueInfo])
    executable = ctx.actions.declare_file(ctx.label.name + "_nextest" + (".exe" if windows else ""))
    nextest = ctx.file._nextest

    ctx.actions.symlink(output = executable, target_file = runner, is_executable = True)

    # The runner's settings, which it reads from its environment.
    settings = {
        "BINARY": to_rlocation_path(ctx, binary),
        "BINARY_ID": ctx.attr.nextest_binary_id,
        "BINARY_KIND": ctx.attr.nextest_kind,
        "BINARY_NAME": ctx.attr.nextest_binary_name,
        "CARGO_METADATA": to_rlocation_path(ctx, ctx.file.cargo_metadata),
        "CONFIG": to_rlocation_path(ctx, ctx.file.nextest_config),
        "NEXTEST": to_rlocation_path(ctx, nextest),

        # The directory of the binary's Cargo package, which the runner finds the package by.
        "PACKAGE": ctx.label.package,
        "TEST_THREADS": str(ctx.attr.test_threads),

        # The platform that the tests run on, which `platform()` filters and `cfg()` overrides in
        # `.config/nextest.toml` match.
        "TRIPLE": ctx.toolchains["@rules_rust//rust:toolchain_type"].target_flag_value,

        # nextest only checks that the workspace's `Cargo.toml` exists. An empty one stands in for
        # it, so that a change to the real one, like a new workspace dependency, doesn't run every
        # test again.
        "WORKSPACE_MANIFEST": ctx.workspace_name + "/Cargo.toml",
    }

    runfiles = ctx.runfiles(
        files = [binary, ctx.file.cargo_metadata, ctx.file.nextest_config, nextest, runner] +
                ctx.files.package_manifest,
        root_symlinks = {
            ctx.workspace_name + "/Cargo.toml": ctx.file._nextest_workspace_manifest,
        },
    ).merge(default_info.default_runfiles)

    # nextest sets `CARGO_MANIFEST_DIR` itself, to the package's directory in the runfiles, which is
    # also the tests' working directory.
    environment = dict(run_environment_info.environment)
    environment.pop("CARGO_MANIFEST_DIR", None)
    environment.update({"BAZEL_NEXTEST_" + name: value for name, value in settings.items()})

    return [
        DefaultInfo(
            executable = executable,
            # `bazel build` of the test still builds the binary.
            files = depset([executable], transitive = [default_info.files]),
            runfiles = runfiles,
        ),
        RunEnvironmentInfo(
            environment = environment,
            inherited_environment = run_environment_info.inherited_environment,
        ),
    ] + [
        provider
        for provider in providers
        if provider != default_info and provider != run_environment_info
    ]

rust_nextest_test = rule(
    implementation = _rust_nextest_test_impl,
    doc = """A `rust_test` whose tests run with cargo-nextest, as `cargo nextest run` runs them.

    The target must be in the package of the `Cargo.toml` that the test belongs to, because the
    runner finds the Cargo package by the target's package. The test's executable is the runner,
    whose `test` subcommand runs the tests, so the target's `args` must start with `test`.

    rules_rust's `rust_test` macro passes the dependencies to both `deps` and `proc_macro_deps`,
    and the rule picks the proc macros from the second and the rest from the first. A caller of
    this rule must do the same.""",
    parent = rust_test,
    attrs = {
        "cargo_metadata": attr.label(
            doc = "The workspace's `cargo_metadata`.",
            allow_single_file = True,
            default = "//:cargo_metadata",
        ),
        "nextest_binary_id": attr.string(
            doc = "The binary's ID in nextest, like `mirrord-layer-tests::c_stat`, which " +
                  "`binary_id()` filters and the JUnit report's test suites take.",
            mandatory = True,
        ),
        "nextest_binary_name": attr.string(
            doc = "The name of the binary's Cargo target, which `binary()` filters.",
            mandatory = True,
        ),
        "nextest_config": attr.label(
            doc = "nextest's configuration, which sets the profiles and the retries.",
            allow_single_file = True,
            default = "//:.config/nextest.toml",
        ),
        "nextest_kind": attr.string(
            doc = "The kind of the binary's Cargo target, which `kind()` filters: `test` for a " +
                  "file in `tests/`, and `lib` or `bin` for a crate's own tests.",
            values = ["bin", "lib", "test"],
            mandatory = True,
        ),
        "package_manifest": attr.label(
            doc = "The package's `Cargo.toml`, which tests that read it relative to their " +
                  "working directory, the package's directory, need in their runfiles.",
            allow_single_file = True,
        ),
        "test_threads": attr.int(
            doc = "How many tests nextest runs at a time, unless the profile sets it. The macro " +
                  "tags the target `cpu:<test_threads>`, so that Bazel reserves as many CPUs for it.",
            default = 1,
        ),
        "_nextest_runner": attr.label(
            default = "//bazel/nextest-runner",
            cfg = "target",
            executable = True,
        ),
        "_nextest_windows": attr.label(default = "@platforms//os:windows"),
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
    },
)
