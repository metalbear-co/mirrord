"""A filegroup that builds its files for an OS and a CPU, such as one slice of a universal binary.

Other rules build an attribute's targets for an OS and a CPU with the filegroup's transition,
`platform_transition`, like `cluster_test` for the images that it loads into a Linux cluster.

A transition that changes `--platforms` builds the files in output directories of their own, even
when the new value names the build's own platform under another label. So the files only get a new
configuration when no existing one fits:

- When the current configuration already targets the OS and CPU, the files keep it, and share its
  outputs. For example, on an arm64 Mac, the universal layer's arm64 slice and `//mirrord/layer` are
  the same build.
- When the machine that runs Bazel has the OS and CPU, the files target `//bazel/platforms:<os>-host`,
  the platform that `module.bazelrc` gives the top-level build, so they share the top-level build's
  outputs. For example, on an x86-64 Linux machine, the x86-64 layer in the arm64 image and
  `//mirrord/layer` are the same build.
"""

load("@platforms//host:constraints.bzl", "HOST_CONSTRAINTS")

_PLATFORMS_FLAG = "//command_line_option:platforms"

def _platform_transition_impl(settings, attr):
    if not attr.target_platform:
        return {_PLATFORMS_FLAG: settings[_PLATFORMS_FLAG]}
    return {_PLATFORMS_FLAG: str(attr.target_platform)}

# Builds an attribute's targets for the platform in the rule's `target_platform` attribute, which
# `transition_platform` gives its value, or for the rule's own platform when that's `None`.
platform_transition = transition(
    implementation = _platform_transition_impl,
    inputs = [_PLATFORMS_FLAG],
    outputs = [_PLATFORMS_FLAG],
)

def _platform_filegroup_impl(ctx):
    return [DefaultInfo(files = depset(transitive = [src[DefaultInfo].files for src in ctx.attr.srcs]))]

_platform_filegroup = rule(
    implementation = _platform_filegroup_impl,
    attrs = {
        "srcs": attr.label_list(cfg = platform_transition),
        "target_platform": attr.label(),
    },
)

# The platform of the machine that runs Bazel, `//bazel/platforms:<os>-host`, which `module.bazelrc`
# gives the top-level build. A transition to it lands in the top-level build's configuration, where
# a transition to `@platforms//host` would create a configuration of its own.
HOST_PLATFORM = str(Label("//bazel/platforms:{}-host".format(
    "macos" if "@platforms//os:osx" in HOST_CONSTRAINTS else "windows" if "@platforms//os:windows" in HOST_CONSTRAINTS else "linux",
)))

def platform_label(os, cpu):
    """The label of the platform that builds for `os` and `cpu`.

    It's `//bazel/platforms:<os>-host` when the machine that runs Bazel has the OS and CPU, so that
    the files share the top-level build's outputs, and `//bazel/platforms:<cpu>-<os>` otherwise.
    Both are this repository's platforms, also when a BUILD file of another module calls the
    function.

    Args:
      os: The OS to build for: `linux` or `macos`.
      cpu: The CPU to build for: `aarch64` or `x86_64`.

    Returns:
      The platform's label, as a string.
    """

    # The host's constraints name macOS by its older name, `osx`, which `macos` is an alias of.
    host_os = "@platforms//os:" + {"macos": "osx"}.get(os, os)
    if host_os in HOST_CONSTRAINTS and "@platforms//cpu:" + cpu in HOST_CONSTRAINTS:
        return str(Label("//bazel/platforms:{}-host".format(os)))
    return str(Label("//bazel/platforms:{}-{}".format(cpu, os)))

def transition_platform(os, cpu):
    """The `target_platform` that makes `platform_transition` build for `os` and `cpu`.

    It's `None`, which keeps the current configuration, when that configuration already targets
    the OS and CPU, and `platform_label(os, cpu)` otherwise.

    Args:
      os: The OS to build for: `linux` or `macos`.
      cpu: The CPU to build for: `aarch64` or `x86_64`.

    Returns:
      A `select` of the platform's label.
    """
    return select({
        Label("//bazel/platforms:is_{}_{}".format(os, cpu)): None,
        "//conditions:default": platform_label(os, cpu),
    })

def platform_filegroup(name, srcs, os, cpu, **kwargs):
    """A filegroup with the files of `srcs`, built for `os` and `cpu`.

    Args:
      name: The filegroup's name.
      srcs: The targets to build.
      os: The OS to build for: `linux` or `macos`.
      cpu: The CPU to build for: `aarch64` or `x86_64`.
      **kwargs: The attributes common to all rules, like `tags`.
    """
    _platform_filegroup(
        name = name,
        srcs = srcs,
        target_platform = transition_platform(os, cpu),
        **kwargs
    )
