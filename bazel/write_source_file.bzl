"""`write_source_file`, which keeps a checked-in file equal to one that the build generates.

Some generated files are checked in, like `mirrord-schema.json`, which editors and the docs read
from the repository. Bazel can't write to the source tree during a build, so `bazel run` of the
target copies the generated file over the checked-in one, and a test fails while the two differ.
`program_stdout` generates such a file from what a program of the build prints.

bazel_lib's `write_source_files` does the same, but on Unix its script looks for the generated file
from its working directory, which it expects to be its runfiles. `module.bazelrc` sets `--run_in_cwd`,
which starts the script in the developer's directory instead, where it finds no file, and then
empties the checked-in directory that it was going to replace. The program here finds the generated
file through the runfiles library, so it works from any directory.
"""

load("@bazel_lib//lib:diff_test.bzl", "diff_test")
load("@rules_python//python:py_binary.bzl", "py_binary")

def write_source_file(name, in_file, out_file):
    """Writes `in_file` over `out_file` on `bazel run`, and tests that they're equal as `<name>_test`.

    Args:
      name: The name of the target that writes the file.
      in_file: The generated file or directory.
      out_file: The checked-in file or directory, in the repository of the calling BUILD file. When
        it's in another package, that package must export it. `bazel run` only writes it when that
        repository is the main one, since the program only knows the main repository's checkout.
    """
    in_label = native.package_relative_label(in_file)
    out_label = native.package_relative_label(out_file)
    out_path = "/".join([part for part in [out_label.package, out_label.name] if part])

    py_binary(
        name = name,
        srcs = [Label(":write_source_file.py")],
        data = [in_file],
        main = Label(":write_source_file.py"),
        args = [
            "$(rlocationpath {})".format(in_file),
            str(out_label),
            out_path,
        ],
        deps = [Label("@rules_python//python/runfiles")],
    )

    diff_test(
        name = name + "_test",
        failure_message = "\n".join([
            # The test's script expands the message in double quotes, so it has no backticks.
            "{} isn't what //{}:{} generates. To write it again, run:".format(
                out_path,
                in_label.package,
                in_label.name,
            ),
            "",
            "    bazel run //{}:{}".format(native.package_name(), name),
        ]),
        file1 = in_file,
        file2 = out_file,
    )

def program_stdout(name, program, out, **kwargs):
    """Writes what `program` prints to `out`.

    `run_binary` would build the program, and the crates that it links, again for the machine that
    runs the action. This genrule takes the program from its `srcs`, which are built for the target
    platform like the rest of `//...`, so the program comes from the build's own outputs. That only
    runs when the target platform is the machine's own OS and CPU, so the target is incompatible
    with any other, such as `--platforms=//bazel/platforms:x86_64-windows` on Linux, and a wildcard
    build skips it there.

    Args:
      name: The genrule's name.
      program: The program to run, without arguments.
      out: The file that gets the program's output.
      **kwargs: The attributes common to all rules, like `visibility`.
    """
    native.genrule(
        name = name,
        srcs = [program],
        outs = [out],
        cmd = "$(execpath {}) > $@".format(program),
        # Windows machines don't always have the shell that `cmd` runs in.
        cmd_bat = "$(execpath {}) > $@".format(program),
        target_compatible_with = select({
            Label("//bazel/platforms:is_host"): [],
            "//conditions:default": ["@platforms//:incompatible"],
        }),
        **kwargs
    )
