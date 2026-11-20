"""Runs a workspace's checks as `bazel run //:lint`, and its formatters as `bazel run //:format`.

A module's own `lint.py` and `format.py` list its checks and formatters, and pass them to `run_lint`
and `run_format` here. So mirrord and every module that depends on it, like the operator, have the
same commands, with the same flags, `--help` and output.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path

from mirrord_tools import rlocation, run

LINT_DESCRIPTION = "Lints the workspace.\n"

LINT_EPILOG = """\
how to use it:
  Run everything before you push:

    bazel run //:lint

  Name some of the entries below to run only those:

    bazel run //:lint -- <name>...

  Or leave some of them out:

    bazel run //:lint -- --skip <name> --skip <name>

  Bazel downloads or builds the tools, so they don't come from the machine. They all run at the
  same time, those with a setup command once it finished, and each prints its output as it
  finishes. The summary at the end names any that failed.
"""

FORMAT_DESCRIPTION = "Formats the workspace.\n"

FORMAT_EPILOG = """\
how to use it:
  Format everything before you commit:

    bazel run //:format

  Check a change without touching any file:

    bazel run //:format -- --check

  Name some of the tools below to run only those:

    bazel run //:format -- <name>...

  Bazel downloads or builds the tools, so they don't come from the machine. Each one runs in turn,
  or all at the same time with --check, those with a setup command once it finished, and the
  summary at the end names any that failed.
"""


@dataclass(frozen=True)
class Setup:
    """A command that must run before some checks or formatters, like `pnpm install` before the
    ones that the frontends' packages provide.

    `title` names the command in the output.
    """

    title: str
    command: list[str]


@dataclass(frozen=True)
class Check:
    """A check that `run_lint` runs.

    `summary` says what the check checks, for `--help`.

    `command` is the program that checks the workspace, with its arguments, and the check passes
    when the program exits with 0. The program is a name on `PATH`, where `put_tools_on_path` puts
    the programs of the target's `RUNNER_TOOLS`, or an absolute path. `setup` runs once before the
    checks that name it.

    With `fails_on_output`, the check also fails when the program prints anything, for a program
    that exits with 0 when it only finds warnings, like `starpls check`.
    """

    summary: str
    command: list[str]
    setup: Setup | None = None
    fails_on_output: bool = False


@dataclass(frozen=True)
class Formatter:
    """A formatter that `run_format` runs.

    `summary` says what the formatter formats, for `--help`. `command` formats the files, and
    `check_command` only checks them, for `--check`. Each one is a program and its arguments, like a
    check's command. `setup` runs once before the formatters that name it.
    """

    summary: str
    command: list[str]
    check_command: list[str]
    setup: Setup | None = None


def put_tools_on_path():
    """Puts the directory of each program that Bazel gives the checks or the formatters at the start
    of `PATH`, so that they, and the programs that they start, run these rather than the machine's.

    The programs are the files of the target's `RUNNER_TOOLS`, which holds their runfiles paths,
    separated by spaces, as `$(rlocationpaths)` gives them. The interpreter that runs this program
    comes first, as `python3`, for the scripts that the checks run.
    """
    directories = [Path(sys.executable).parent] + [
        Path(rlocation(tool)).parent for tool in os.environ.get("RUNNER_TOOLS", "").split()
    ]
    os.environ["PATH"] = os.pathsep.join([*dict.fromkeys(map(str, directories)), os.environ["PATH"]])


def listing(title, summaries):
    """Returns a section of a `--help` epilog that lists names, like a script's checks, with their
    summaries."""
    width = max(map(len, summaries))
    return f"\n{title}:\n" + "\n".join(f"  {name:<{width}}  {summary}" for name, summary in summaries.items())


def git_files(*pathspecs):
    """Returns the paths of the workspace's files that git tracks and that match `pathspecs`, for a
    check's command."""
    git = subprocess.run(
        ["git", "ls-files", *pathspecs],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
        cwd=os.environ["BUILD_WORKSPACE_DIRECTORY"],
    )
    return git.stdout.splitlines()


def starpls_check():
    """Returns the check of the types of the workspace's Starlark code with starpls, as the editors
    that use starpls as their language server check them.

    starpls finds a file that a `load` names in another repository in Bazel's output base, which
    only has the repositories that an earlier command needed. So on a fresh checkout, like CI's, it
    warns about every load from the repositories of module extensions, like the crate hub. The
    check's setup fetches every repository that a file loads from first.

    starpls exits with 0 when it only finds warnings, so any output fails the check.
    """
    files = git_files("*.bzl", "*.bazel")
    root = os.environ["BUILD_WORKSPACE_DIRECTORY"]
    repositories = {
        name for file in files for name in re.findall(r'load\(\s*"(@[^/"]+)//', Path(root, file).read_text())
    }
    # A load from the module's own name is a load from the workspace, which `bazel fetch` rejects.
    module = re.search(r'module\(\s*name\s*=\s*"([^"]+)"', Path(root, "MODULE.bazel").read_text())
    if module:
        repositories.discard(f"@{module[1]}")
    return Check(
        "the types of the Starlark code",
        ["starpls", "check", *files],
        setup=Setup("bazel fetch", ["bazel", "fetch", *(f"--repo={name}" for name in sorted(repositories))]),
        fails_on_output=True,
    )


def ty_command():
    """Returns the command that type-checks the workspace's Python code with ty, against the
    packages that the lint runs with.

    ty resolves imports from the `sys.path` entries in the runfiles, which hold the `deps` of the
    lint's target, so the target must depend on every package that the workspace's code imports.

    The interpreter's own entries are left out, because its standard library would hide ty's stubs
    for it. ty gets the interpreter from `--python` instead.
    """
    # This file is `<repository>/scripts/mirrord_runner.py` in the runfiles.
    runfiles = Path(__file__).parents[2]
    interpreter = [Path(sys.prefix), Path(sys.base_prefix)]
    search_paths = [
        path
        for path in map(Path, sys.path)
        if path.is_relative_to(runfiles) and not any(path.is_relative_to(prefix) for prefix in interpreter)
    ]
    return [
        "ty",
        "check",
        "--python",
        sys.base_prefix,
        *(f"--extra-search-path={path}" for path in search_paths),
    ]


def run_captured(command):
    """Runs the command, and returns whether it passed and what it printed. A command that runs at
    the same time as others prints this way, so that their outputs don't mix."""
    with tempfile.TemporaryFile("w+", encoding="utf-8", errors="replace") as output:
        passed = run(command, output)
        output.seek(0)
        return passed, output.read()


def start_setups(entries, pool):
    """Starts the setup of each of `entries`, the checks or formatters that will run, once, in
    `pool`, and returns a function that waits for an entry's setup and gives the message for an
    entry whose setup failed, or `None` when the entry can run.

    The entries that have no setup don't wait for the setups, so they run while `pnpm install`
    does. Each setup prints its output when it ends, as a check does."""

    def run_setup(setup):
        passed, output = run_captured(setup.command)
        print(f"=== {setup.title}\n{output}", end="", flush=True)
        return passed

    setups = {}
    for entry in entries:
        if entry.setup is not None and entry.setup.title not in setups:
            setups[entry.setup.title] = pool.submit(run_setup, entry.setup)

    def setup_failure(entry):
        if entry.setup is not None and not setups[entry.setup.title].result():
            return f"It didn't run, because `{entry.setup.title}` failed.\n"
        return None

    return setup_failure


def run_at_once(names, run_one, pool):
    """Runs `run_one` for each of `names` in `pool`, prints each one's output when it ends, and
    returns the names whose run failed. `run_one` returns whether it passed and its output.

    `pool` must start the setups of `start_setups` before these runs, which it does when they
    were submitted first, so that a run that waits for a setup never takes the setup's thread."""
    failed = []
    started = {pool.submit(run_one, name): name for name in names}
    for ended in as_completed(started):
        passed, output = ended.result()
        print(f"=== {started[ended]}\n{output}", end="", flush=True)
        if not passed:
            failed.append(started[ended])
    return failed


def run_lint(checks: dict[str, Check]):
    """Runs the checks, or the ones that the command line names, at the same time, prints each one's
    output when it ends, and exits with 1 if any failed."""
    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])
    # ty would also resolve imports from the devshell's `PYTHONPATH`, which Bazel doesn't use.
    os.environ.pop("PYTHONPATH", None)
    put_tools_on_path()

    parser = argparse.ArgumentParser(
        prog="bazel run //:lint --",
        description=LINT_DESCRIPTION,
        epilog=LINT_EPILOG + listing("the checks", {name: check.summary for name, check in checks.items()}),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "checks",
        nargs="*",
        metavar="name",
        help="run only these, from the list below, rather than all of them",
    )
    parser.add_argument(
        "--skip",
        action="append",
        default=[],
        metavar="name",
        help="leave out this check, from the list below; repeat it to leave out several",
    )
    arguments = parser.parse_args()

    unknown = [name for name in [*arguments.checks, *arguments.skip] if name not in checks]
    if unknown:
        parser.error(f"unknown checks {unknown}, choose from {list(checks)}")
    names = [name for name in arguments.checks or checks if name not in arguments.skip]

    def run_check(name):
        failure = setup_failure(checks[name])
        if failure is not None:
            return False, failure
        passed, output = run_captured(checks[name].command)
        return passed and not (checks[name].fails_on_output and output), output

    with ThreadPoolExecutor() as pool:
        setup_failure = start_setups([checks[name] for name in names], pool)
        failed = run_at_once(names, run_check, pool)
    if failed:
        print("\nThese checks failed:")
        for name in sorted(failed):
            print(f"  {name}")
        sys.exit(1)


def run_format(formatters: dict[str, Formatter]):
    """Runs the formatters, or the ones that the command line names, one after the other, and exits
    with 1 if any failed.

    With `--check`, the formatters only check the files. They change none, so they all run at the
    same time, and each one's output is printed when it ends, as `run_lint` does with its checks."""
    parser = argparse.ArgumentParser(
        prog="bazel run //:format --",
        description=FORMAT_DESCRIPTION,
        epilog=FORMAT_EPILOG
        + listing("the formatters", {name: formatter.summary for name, formatter in formatters.items()}),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="leave the files as they are, and fail if one needs changes",
    )
    parser.add_argument(
        "formatters",
        nargs="*",
        metavar="name",
        help="run only these, from the list below, rather than all of them",
    )
    arguments = parser.parse_args()

    unknown = [name for name in arguments.formatters if name not in formatters]
    if unknown:
        parser.error(f"unknown formatters {unknown}, choose from {list(formatters)}")
    names = arguments.formatters or list(formatters)

    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])
    put_tools_on_path()

    with ThreadPoolExecutor() as pool:
        setup_failure = start_setups([formatters[name] for name in names], pool)

        if arguments.check:

            def check(name):
                failure = setup_failure(formatters[name])
                if failure is not None:
                    return False, failure
                return run_captured(formatters[name].check_command)

            failed = run_at_once(names, check, pool)
        else:
            failed = []
            for name in names:
                failure = setup_failure(formatters[name])
                print(f"=== {name}", flush=True)
                if failure is not None:
                    print(failure, end="", flush=True)
                    failed.append(name)
                elif not run(formatters[name].command):
                    failed.append(name)

    if failed:
        print("\nThese formatters failed:")
        # In the formatters' own order, also when they ended in another one.
        for name in [name for name in names if name in failed]:
            print(f"  {name}")
        sys.exit(1)
