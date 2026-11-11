"""A crate's `OUT_DIR`, from files that Bazel generates, without a build script.

rules_rust gives a crate the `OUT_DIR` of a dependency that provides rules_rust's `BuildInfo`,
which is normally the crate's build script. When Bazel generates the files itself, like
`frida-gum-sys`'s bindings, this rule provides the directory directly, so no build script has to
be compiled and run only to copy the files there.
"""

load("@rules_rust//rust:rust_common.bzl", "BuildInfo")

def _out_dir_impl(ctx):
    return [
        DefaultInfo(files = depset([ctx.file.dir])),
        BuildInfo(
            out_dir = ctx.file.dir,
            build_script_data = depset(),
            compile_data = depset(),
            bin_link_flags = None,
            cdylib_link_flags = None,
            dep_env = None,
            flags = None,
            link_search_paths = None,
            linker_flags = None,
            rustc_env = None,
        ),
    ]

out_dir = rule(
    implementation = _out_dir_impl,
    doc = "Makes a directory the `OUT_DIR` of every crate that depends on this target.",
    attrs = {
        "dir": attr.label(
            doc = "The directory, for example from `copy_to_directory`.",
            allow_single_file = True,
            mandatory = True,
        ),
    },
)
