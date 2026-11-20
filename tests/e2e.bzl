"""`cluster_test`, an end-to-end test target that prepares the cluster that its tests run on.

End-to-end tests need a cluster with the images that Bazel built, like the agent's. Each target
names its cluster, and `--//tests:cluster_runtime` picks the tool that runs it, kind by
default, so the
target's executable, `:e2e_runner`, knows which cluster to create or reuse without reading it from a
kubeconfig. The runner loads the target's images into the cluster, runs the target's setup program,
and then runs the target's tests with that cluster's kubeconfig, and with the target's own
arguments, like the nextest profile that picks its tests.

The tests, the CLI and the layer build for the machine that runs the tests, Linux or macOS, while
the images build for Linux, because the cluster runs them on its nodes. kind and minikube run the
nodes as containers on the machine, so the images build for the machine's CPU too: a Mac with an
Apple chip runs arm64 Linux containers.

Other modules define their own end-to-end suites with this macro, like the operator, whose setup
installs the operator in the cluster. The runtime flag is mirrord's for every module, because the
tool that runs the clusters is a property of the developer's machine rather than of a suite.
"""

load("@bazel_lib//lib:paths.bzl", "to_rlocation_path")
load("@platforms//host:constraints.bzl", "HOST_CONSTRAINTS")
load("@rules_img//img:load.bzl", "image_load")
load("//bazel:platform.bzl", "platform_transition", "transition_platform")

# The CPU of the cluster's nodes, which kind and minikube run as containers on this machine.
_NODE_CPU = "aarch64" if "@platforms//cpu:aarch64" in HOST_CONSTRAINTS else "x86_64"

# The tags of every `cluster_test`, whose tests use and change a cluster that Bazel can't see.
CLUSTER_TEST_TAGS = [
    # Bazel runs each target by itself, with no other test at the same time, so two targets never
    # prepare the same cluster at the same time.
    "exclusive",
    # Bazel never caches the tests' results.
    "external",
    # `//...` leaves the tests out, so that testing `//...` doesn't create clusters.
    "manual",
    # Bazel's sandbox makes the user's home read-only, and minikube keeps its profiles there.
    "no-sandbox",
    # The tests run on this machine, with network access, so they can reach the cluster.
    "no-remote-exec",
    "requires-network",
]

def _cluster_test_impl(ctx):
    tests = ctx.attr.tests
    launcher = ctx.attr._launcher[DefaultInfo]

    # The runner would give two images with the same variable one name.
    variables = ctx.attr.image_variables
    for index, variable in enumerate(variables):
        if variable in variables[:index]:
            fail("Two images of `images` have the variable {}. Each image needs its own.".format(variable))

    # A rule's executable must be a file that the rule creates, so the test's executable is a link
    # to the launcher.
    executable = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(
        output = executable,
        target_file = launcher.files_to_run.executable,
        is_executable = True,
    )

    # rules_img's `image_load` builds the archive that `docker save` would write, with the image's
    # name in its `manifest.json`, only in this output group.
    image_archives = {
        variable: image[OutputGroupInfo].tarball
        for image, variable in ctx.attr.images.items()
    }

    # The runner reads the target's files from these settings, by their paths in the runfiles. The
    # paths hold each file's repository, so the target can be in any module.
    settings = {
        "image_archives": {
            variable: to_rlocation_path(ctx, archive.to_list()[0])
            for variable, archive in image_archives.items()
        },
        "image_variables": variables,
        "tests": to_rlocation_path(ctx, tests[DefaultInfo].files_to_run.executable),
        "tools": [
            to_rlocation_path(ctx, tool[DefaultInfo].files_to_run.executable)
            for tool in ctx.attr._tools
        ],
    }
    transitive_runfiles = [
        launcher.default_runfiles,
        tests[DefaultInfo].default_runfiles,
    ] + [data[DefaultInfo].default_runfiles for data in ctx.attr.data + ctx.attr._tools]

    if ctx.attr.setup:
        setup = ctx.attr.setup
        settings["setup"] = {
            "command": [to_rlocation_path(ctx, setup[DefaultInfo].files_to_run.executable)] + ctx.attr.setup_args,
            # The environment that `bazel run` would give the program, like the runfiles paths
            # that a `py_binary` sets with `$(rlocationpath)` in its `env`.
            "environment": setup[RunEnvironmentInfo].environment if RunEnvironmentInfo in setup else {},
        }
        transitive_runfiles.append(setup[DefaultInfo].default_runfiles)

    runfiles = ctx.runfiles(
        files = [ctx.file._ipv6_kind_config] + ctx.files.data,
        transitive_files = depset(transitive = image_archives.values()),
    ).merge_all(transitive_runfiles)

    return [
        DefaultInfo(executable = executable, runfiles = runfiles),
        # The environment of the tests, which the launcher passes on to them, like the settings of
        # the program that runs nextest.
        RunEnvironmentInfo(
            environment = tests[RunEnvironmentInfo].environment | {
                "CLUSTER_TEST_SETTINGS": json.encode(settings),
            },
            # kind and minikube reach Docker through `DOCKER_HOST` on the machines that set it, and
            # minikube keeps its profiles in `MINIKUBE_HOME` when it's set.
            inherited_environment = tests[RunEnvironmentInfo].inherited_environment + [
                "DOCKER_HOST",
                "MINIKUBE_HOME",
            ],
        ),
    ]

_cluster_test = rule(
    implementation = _cluster_test_impl,
    doc = "Runs `:e2e_runner` with the environment and the runfiles of `tests`.",
    test = True,
    attrs = {
        "data": attr.label_list(
            doc = "The files that the tests read, like their apps, beyond those of `tests`.",
            allow_files = True,
        ),
        "image_variables": attr.string_list(
            doc = "The environment variables that give the tests the names of their images. " +
                  "The list also has the variables of the images that the target doesn't load.",
        ),
        "images": attr.label_keyed_string_dict(
            doc = "The `image_load` targets of the images to load into the cluster, each with " +
                  "the variable that gives the tests the image's name. They build for " +
                  "`target_platform`.",
            cfg = platform_transition,
            providers = [OutputGroupInfo],
        ),
        "setup": attr.label(
            doc = "The program that prepares the cluster after the images are loaded.",
            cfg = "target",
            executable = True,
        ),
        "setup_args": attr.string_list(doc = "The arguments of `setup`."),
        "target_platform": attr.label(
            doc = "The platform of the cluster's nodes, which `images` build for, as " +
                  "`transition_platform` gives it.",
        ),
        "tests": attr.label(
            doc = "The test target that runs on the cluster, like a `crate.unit_tests` tagged " +
                  "`manual`, or a `rust_nextest_suite`.",
            mandatory = True,
            providers = [RunEnvironmentInfo],
        ),
        "_ipv6_kind_config": attr.label(
            allow_single_file = True,
            default = Label("//tests:kind-cluster-ipv6-config.yaml"),
        ),
        "_launcher": attr.label(
            default = Label("//tests:e2e_runner"),
            cfg = "target",
            executable = True,
        ),
        # The programs that the runner, the tests and `setup` run, which the runner puts on `PATH`,
        # so that they don't come from the machine.
        "_tools": attr.label_list(
            default = [
                Label("@multitool//tools/helm"),
                Label("@multitool//tools/kind"),
                Label("@multitool//tools/kubectl"),
                Label("@multitool//tools/minikube"),
            ],
            cfg = "target",
        ),
    },
)

def _opt_transition_impl(_settings, _attr):
    return {"//command_line_option:compilation_mode": "opt"}

_opt_transition = transition(
    implementation = _opt_transition_impl,
    inputs = [],
    outputs = ["//command_line_option:compilation_mode"],
)

def _optimized_image_load_impl(ctx):
    return ctx.super()

optimized_image_load = rule(
    implementation = _optimized_image_load_impl,
    doc = "rules_img's `image_load`, which builds the image with `-c opt`, so that a " +
          "`cluster_test` loads the image that a release builds, whatever the compilation mode " +
          "of the test.",
    parent = image_load,
    cfg = _opt_transition,
)

def cluster_test(
        name,
        tests,
        cluster,
        images = {},
        setup = None,
        setup_args = [],
        ipv6 = False,
        argo_rollouts = False,
        args = [],
        tags = [],
        **kwargs):
    """Runs `tests` on the cluster `cluster`.

    `--//tests:cluster_runtime` picks the tool that runs the cluster, `kind` by default.
    Bazel downloads kind and minikube, and the kubectl and helm that the runner, the tests and
    `setup` run from `PATH`, so the machine only needs Docker. With `kind` or `minikube`, the target
    prepares the cluster every time it runs: it creates the cluster if it doesn't exist, installs
    Argo Rollouts if `argo_rollouts` is set, loads `images` and runs `setup`. So `setup` must work
    on a cluster that it already set up. With `none`, the tests run on the cluster in `KUBECONFIG`,
    which the developer prepared.

    Each target gets only the files that its own tests read, in `data`, so that a target doesn't
    build the apps of another target's tests.

    The target runs on Linux and macOS. Its tests build for the machine, and `images` build for
    Linux on the machine's CPU, whatever the target platform.

    Args:
      name: The target's name.
      tests: The test target that runs on the cluster, with its runfiles and its environment, like a
        `crate.unit_tests` tagged `manual`, or a `rust_nextest_suite` that runs the tests of several
        binaries in one nextest run.
      cluster: The name of the kind cluster, or of the minikube profile, that the tests run on.
      images: The images to load into the cluster, as a dict from each image's `image_load` target
        to the name of an environment variable. The tests and `setup` read the image's name and
        tag, which the `image_load` target sets, from that variable. When the variable is already in
        the tests' environment, the target doesn't load that image, and the tests run the image that
        the variable names. With the runtime `none`, every variable must be in the tests'
        environment.
      setup: The program that prepares the cluster after the images are loaded, like one that
        installs a Helm chart. It runs with the cluster's `KUBECONFIG` and the variables of
        `images`. It can give the tests variables of their own, like the kubeconfig of a user that
        the setup created, as a JSON object of strings in the file that
        `CLUSTER_TEST_ENVIRONMENT_FILE` names.
      setup_args: The arguments of `setup`.
      ipv6: Whether the cluster is IPv6-only, which only kind creates.
      argo_rollouts: Whether the tests need Argo Rollouts in the cluster, because they target
        rollouts.
      args: The arguments of the tests, like the nextest profile.
      tags: The target's tags, which the macro adds to `CLUSTER_TEST_TAGS`.
      **kwargs: The attributes of a test, like `timeout` and `data`.
    """
    runtime_none = Label("//tests:cluster_runtime_none")
    runtimes = {
        Label("//tests:cluster_runtime_kind"): ["--runtime", "kind"],
        runtime_none: ["--runtime", "none"],
    }
    if not ipv6:
        runtimes[Label("//tests:cluster_runtime_minikube")] = ["--runtime", "minikube"]

    cluster_options = []
    if ipv6:
        cluster_options.append("--ipv6")
    if argo_rollouts:
        cluster_options.append("--argo-rollouts")

    _cluster_test(
        name = name,
        tests = tests,
        # The launcher takes everything after `--` as the arguments of the tests, which is also
        # where `--test_arg` adds its own.
        args = select(
            runtimes,
            no_match_error = "The target needs an IPv6-only cluster, which only kind creates.",
        ) + ["--cluster", cluster] + cluster_options + ["--"] + args,
        # The runtime `none` uses neither the images nor `setup`, so it doesn't build them. The
        # runner still checks that the developer set the images' variables.
        images = select({
            runtime_none: {},
            "//conditions:default": images,
        }),
        image_variables = images.values(),
        setup = select({
            runtime_none: None,
            "//conditions:default": setup,
        }),
        setup_args = setup_args,
        target_platform = transition_platform("linux", _NODE_CPU),
        tags = CLUSTER_TEST_TAGS + tags,
        **kwargs
    )
