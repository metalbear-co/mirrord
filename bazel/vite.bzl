"""`vite_site`, a site that Vite builds, with a test that the site ships no source map.

mirrord's UI and the sites of the modules that depend on mirrord, like the operator's, use it.
"""

load("@bazel_lib//lib:transitions.bzl", "platform_transition_filegroup")
load("@rules_python//python:py_test.bzl", "py_test")
load(":platform.bzl", "HOST_PLATFORM")

def vite_site(name, vite, args = [], visibility = None, **kwargs):
    """Builds a site with `vite build`, into the directory `name`.

    Vite runs native modules, like rollup's, which rules_js picks for the target platform, so the
    site builds for the host, where Vite runs, even when the program or the image that ships it
    builds for another platform. The site is the same on every platform. On the host, the site
    builds in the top-level build's configuration (see `HOST_PLATFORM`).

    A site that ships its source maps publishes its unminified source, so the test
    `<name>_no_source_maps_test` fails when the site has one.

    Args:
      name: The name of the site's target, and of the directory that Vite writes the site to.
      vite: The `vite` rule of the package's `vite/package_json.bzl`, from rules_js.
      args: More arguments of `vite build`.
      visibility: The visibility of the site's target.
      **kwargs: The other attributes of the `vite` rule, like `srcs` and `env`.
    """
    vite(
        name = name + "_for_target",
        out_dirs = [name],
        args = [
            "build",
            "--outDir",
            name,
        ] + args,
        chdir = native.package_name(),
        # The site only builds through the target below, for the host.
        tags = ["manual"],
        **kwargs
    )

    platform_transition_filegroup(
        name = name,
        srcs = [":{}_for_target".format(name)],
        target_platform = HOST_PLATFORM,
        visibility = visibility,
    )

    py_test(
        name = name + "_no_source_maps_test",
        srcs = [Label("//bazel:no_source_maps_test.py")],
        data = [":" + name],
        main = Label("//bazel:no_source_maps_test.py"),
        args = ["$(rootpath :{})".format(name)],
        size = "small",
    )
