"""Writes a file or a directory that the build generates over the checked-in one. It's the program
of the `write_source_file` targets of `write_source_file.bzl`, which pass it the generated file's
path in the runfiles, and the checked-in file's label and its path in its repository."""

import os
import shutil
import sys
from pathlib import Path

from python.runfiles import Runfiles


def main():
    generated_path, checked_in_label, checked_in_path = sys.argv[1:]

    workspace = os.environ.get("BUILD_WORKSPACE_DIRECTORY")
    runfiles = Runfiles.Create()
    generated = runfiles.Rlocation(generated_path) if runfiles else None
    if workspace is None or generated is None:
        sys.exit("The program writes to the workspace, so run it with `bazel run`.")

    # `BUILD_WORKSPACE_DIRECTORY` is the main repository's checkout, so a file of another
    # repository, like a dependency's checked-in file, would land at the same path in the wrong
    # checkout. The canonical label of a file in the main repository starts with `@@//`.
    if not checked_in_label.startswith("@@//"):
        sys.exit(
            f"{checked_in_label} isn't in the repository of the workspace that `bazel run` runs "
            f"in, {workspace}, so run the target from the checkout of its own repository."
        )

    # Bazel makes the generated files and directories read-only, so the program copies their
    # contents without their permissions, which `shutil.copytree` would keep for the directories.
    generated = Path(generated).resolve()
    checked_in = Path(workspace, checked_in_path)

    if not generated.is_dir():
        checked_in.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(generated, checked_in)
        return

    # Removes the checked-in files that the build no longer generates.
    shutil.rmtree(checked_in, ignore_errors=True)
    for directory, _, files in os.walk(generated):
        checked_in_directory = checked_in / Path(directory).relative_to(generated)
        checked_in_directory.mkdir(parents=True)
        for file in files:
            shutil.copyfile(Path(directory, file), checked_in_directory / file)


if __name__ == "__main__":
    main()
