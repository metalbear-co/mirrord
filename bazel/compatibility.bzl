"""Values for `target_compatible_with`, for targets that build or run only on some OSes.

A target that isn't compatible with the platform that a build targets is skipped by `bazel build`
and `bazel test` with a pattern like `//...`, and fails a build that names it. Each value mirrors a
`cfg` in the sources, like `#![cfg(unix)]`. The `cfg` must stay, because Cargo builds every target
for every platform.

Libraries mustn't use these: rules_rust also analyzes every dependency of a crate for the platform
that runs the build, to find proc macros, so an incompatible library would make every crate that
depends on it incompatible when the build targets another platform. Their dependents already take
them only on the right platforms, through the `select()`s that rules_rs generates from Cargo's
`[target.'cfg(...)'.dependencies]`.
"""

LINUX_ONLY = ["@platforms//os:linux"]

MACOS_ONLY = ["@platforms//os:macos"]

WINDOWS_ONLY = ["@platforms//os:windows"]

# The end-to-end tests run on Linux and Windows machines, against a cluster.
LINUX_OR_WINDOWS = select({
    "@platforms//os:linux": [],
    "@platforms//os:windows": [],
    "//conditions:default": ["@platforms//:incompatible"],
})

UNIX_ONLY = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})
