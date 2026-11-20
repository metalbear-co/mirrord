import functools
import os
import shutil
import subprocess
import sys
from pathlib import Path

from python.runfiles import Runfiles

PNPM_INSTALL = ["pnpm", "install", "--frozen-lockfile", "--silent"]

# The program's runfiles, or `None` when it has none. The call is cached, because it reads the
# runfiles' `_repo_mapping` file.
find_runfiles = functools.cache(Runfiles.Create)


def run(command, output=None, env=None):
    program, *arguments = command
    # On Windows, `shutil.which` also tries the extensions in `PATHEXT`, so it finds `pnpm.cmd`,
    # which `subprocess.run` doesn't.
    path = shutil.which(program)
    if path is None:
        print(f"`{program}` isn't on PATH.", file=output or sys.stdout, flush=True)
        return False
    process = subprocess.run(
        [path, *arguments], stdout=output, stderr=subprocess.STDOUT if output else None, env=env, check=False
    )
    return process.returncode == 0


def rlocation(runfiles_path):
    """Returns the absolute path of the file at the runfiles path `runfiles_path`, which a Bazel
    target passes with `$(rlocationpath)` for a file in another repository, like rustfmt. The path's
    first part can also be a repository's name as mirrord's `MODULE.bazel` declares it, like
    `mirrord`.

    The program exits when the file isn't in its runfiles, because its target's `data` must list
    the file."""
    runfiles = find_runfiles()
    path = runfiles.Rlocation(runfiles_path) if runfiles else None
    if path is None or not Path(path).exists():
        sys.exit(f"{runfiles_path} isn't in the program's runfiles. Run the program with `bazel run`.")
    return path


def runfile(path):
    """Returns the absolute path of the file at `path` in mirrord's repository, like
    `tests/kind-cluster-ipv6-config.yaml`, in the program's runfiles.

    The runfiles hold mirrord's files in `_main` when mirrord is the main repository, and in
    `mirrord+` when another Bazel module depends on it. So the path names the module, `mirrord`,
    which the runfiles library maps to the directory as this file's repository sees it."""
    return rlocation(f"mirrord/{path}")


def runfile_program(path):
    """Returns the absolute path of the program at `path` in mirrord's repository, like
    `medschool/medschool`, in the program's runfiles, with the `.exe` that Bazel adds to every
    program's name on Windows."""
    return runfile(path + (".exe" if os.name == "nt" else ""))
