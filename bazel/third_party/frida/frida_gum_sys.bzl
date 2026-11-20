"""`frida_gum_sys`, which does the work of the `frida-gum-sys` crate's build script.

The build script would download Frida's devkit, compile the crate's C code against it, and generate
the bindings with bindgen, which needs libclang and the machine's headers. So the crate's annotation
turns the build script off, and these targets do its work with the devkit that Bazel downloads and
with rules_rs's hermetic bindgen.

The annotation in mirrord's `MODULE.bazel` calls the macro in the crate's own BUILD file, so each
crate hub builds the C code and the bindings of the version that it locks. A module that builds the
crate in its own hub annotates the crate in the same way.
"""

load("@bazel_lib//lib:copy_to_directory.bzl", "copy_to_directory")
load("@rules_cc//cc:cc_library.bzl", "cc_library")
load("@rules_rs//rs:rules_rust_bindgen.bzl", "rust_bindgen")
load("//bazel:out_dir.bzl", "out_dir")

def frida_gum_sys(name):
    """The dependencies that replace the build script of the `frida-gum-sys` crate.

    The crate's annotation lists two of the macro's targets in its `deps`: `<name>_listeners`, the
    crate's C code, and `<name>_out_dir`, the `$OUT_DIR/bindings.rs` that the build script would
    write.

    Args:
      name: The prefix of the targets' names.
    """
    cc_library(
        name = name + "_listeners",
        srcs = [
            "invocation_listener.c",
            "probe_listener.c",
        ],
        hdrs = native.glob(["*.h"]),
        copts = ["-O3"],
        includes = ["."],
        local_defines = ["USE_GUM_JS=0"],
        deps = [Label("//bazel/third_party/frida:devkit")],
        visibility = ["//visibility:public"],
    )

    # The build script's own bindgen arguments.
    rust_bindgen(
        name = name + "_bindings",
        bindgen_flags = [
            "--blocklist-type=GumChainedPtr64Rebase",
            "--blocklist-type=GumChainedPtrArm64eRebase",
            "--blocklist-type=_GumChainedPtr64Rebase",
            "--blocklist-type=_GumChainedPtrArm64eRebase",
            "--no-doc-comments",
            "--no-layout-tests",
            "--use-core",
        ],
        cc_lib = ":" + name + "_listeners",
        clang_flags = [
            # The toolchain redefines `__DATE__` and `__TIME__`, and rust_bindgen passes the
            # definitions on to clang but drops the flag that turns off clang's warning about them.
            "-Wno-builtin-macro-redefined",
            # rust_bindgen passes `_FORTIFY_SOURCE` without the optimization level that glibc
            # requires.
            "-U_FORTIFY_SOURCE",
            "-DUSE_GUM_JS=0",
            "-include",
            "frida-gum.h",
            "-include",
            "event_sink.h",
            "-include",
            "invocation_listener.h",
            "-include",
            "probe_listener.h",
            "-include",
            "stalker_observer.h",
        ],
        header = "stalker_params.h",
    )

    # rust_bindgen names its file after the target, and the crate includes `bindings.rs`.
    copy_to_directory(
        name = name + "_bindings_dir",
        srcs = [":" + name + "_bindings"],
        replace_prefixes = {name + "_bindings.rs": "bindings.rs"},
    )

    out_dir(
        name = name + "_out_dir",
        dir = ":" + name + "_bindings_dir",
        visibility = ["//visibility:public"],
    )
