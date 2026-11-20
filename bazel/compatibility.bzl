"""Values for `target_compatible_with`, for the targets that only build or run on some OSes.

A target that uses one of these values keeps the matching `cfg` in its sources too, like
`#![cfg(unix)]`, because Cargo builds every target for every platform.

Libraries shouldn't use these values, because rules_rust analyzes every dependency for the
execution platform too, to find the proc macros. So in a cross build, an incompatible library would
make every crate that depends on it incompatible.

Libraries don't need these values either: the crates that depend on a library already take it only
on the right platforms, through the `select()`s that rules_rs generates from Cargo's
`[target.'cfg(...)'.dependencies]`.
"""

LINUX_ONLY = ["@platforms//os:linux"]

MACOS_ONLY = ["@platforms//os:macos"]

WINDOWS_ONLY = ["@platforms//os:windows"]

UNIX_ONLY = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})
