"""`runfiles_link`, a link that gives a file a path in the runfiles that code can write down.

Code that reads a file from its runfiles names the file's path there. Some files have no path that
the code can write down:

- A file from another repository has a path that starts with the repository's canonical name, which
  isn't stable.
- A file that a `select()` picks has a different path on each platform.

The link puts such a file at `<package>/<name>` in the runfiles, with the file's own extension. The
code then names that path, and adds the extension that the platform gives the file, like `.dll` or
`.exe` on Windows.

`copy_file`, from bazel_lib or skylib, names its output in `out`, which a `select()` can't change,
so a file whose extension differs between platforms would need a `copy_file` for each platform.
`copy_file` also refuses a directory. `copy_directory` takes one, but copies it file by file, where
the link is a single symlink.
"""

def _runfiles_link_impl(ctx):
    src = ctx.file.src
    name = ctx.label.name + ("." + src.extension if src.extension else "")
    link = ctx.actions.declare_directory(name) if src.is_directory else ctx.actions.declare_file(name)
    ctx.actions.symlink(output = link, target_file = src)
    return [DefaultInfo(files = depset([link]))]

runfiles_link = rule(
    implementation = _runfiles_link_impl,
    doc = "A link to `src` at `<package>/<name>` in the runfiles, with the extension of `src`.",
    attrs = {
        "src": attr.label(
            doc = "The file or directory, like the program of a toolchain's repository.",
            allow_single_file = True,
            mandatory = True,
        ),
    },
)
