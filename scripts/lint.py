"""Runs the checks of the workspace that a Bazel build doesn't run. A build with `--config=clippy`
already lints the Rust code, so these are the other checks:

- typos, Vale and markdownlint on the docs;
- towncrier, which checks that the branch adds a changelog fragment;
- cargo-deny on the dependencies' licenses and advisories, and cargo-shear for unused
  dependencies, which matter to Bazel too, because it builds every dependency that a `Cargo.toml`
  lists;
- the tests of the scripts behind the flaky test report;
- Ruff on the Python code, with the settings in `ruff.toml`;
- ESLint and the TypeScript compiler on the frontends.

It runs as `bazel run //:lint`, which runs it in the workspace, and takes the tools from `PATH`, as
the devshell provides them. The checks run at the same time, because none of them writes a file
that another one reads, except that ESLint and the TypeScript compiler wait for pnpm to install
them. Each check's output prints when the check ends, so that the outputs don't mix. Every check
runs, even after one fails, and the failed ones are listed at the end.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor, as_completed

from scripts.tools import FRONTENDS, PNPM_INSTALL, run


def git_files(*pathspecs):
    git = subprocess.run(
        ["git", "ls-files", *pathspecs], stdout=subprocess.PIPE, text=True, check=True
    )
    return git.stdout.splitlines()


def checks():
    """Returns each check's command by its name, and the names of the frontends' checks, which
    run the tools that the `pnpm-install` check installs."""
    commands = {
        # The docs and skills vendored for `mirrord mcp` are upstream's, kept byte-identical, so
        # the linters leave them alone.
        "typos": [
            "typos",
            "--format",
            "brief",
            *git_files("*.md", ":!mirrord/mcp/corpus", "mirrord/config/*.rs"),
        ],
        # Vale checks the root Markdown files, except the changelog that the release pull request
        # builds, and the changelog fragments that become it. `report.tmpl` is in
        # `.vale/styles/config/templates`.
        "vale": [
            "vale",
            "--output=report.tmpl",
            *git_files(":(glob)*.md", ":!CHANGELOG.md", "changelog.d/*.md"),
        ],
        "markdownlint": ["markdownlint-cli2", "README.md"],
        # towncrier compares the branch with `origin/main`.
        "towncrier": ["towncrier", "check"],
        "cargo-deny": ["cargo", "deny", "--all-features", "check"],
        "cargo-shear": ["cargo", "shear", "--deny-warnings"],
        "ruff": ["ruff", "check"],
    }

    # The tests fake `gh` and `curl` with scripts, which only run on Unix, like the flaky test
    # report's own scripts.
    if os.name != "nt":
        commands["flaky-report-tests"] = [sys.executable, ".github/scripts/test_flaky_report.py"]

    commands["pnpm-install"] = PNPM_INSTALL
    frontend_checks = set()
    for package in FRONTENDS:
        commands[f"eslint-{package}"] = ["pnpm", "--filter", package, "run", "lint"]
        commands[f"typecheck-{package}"] = ["pnpm", "--filter", package, "run", "typecheck"]
        frontend_checks |= {f"eslint-{package}", f"typecheck-{package}"}

    return commands, frontend_checks


def main():
    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])
    commands, frontend_checks = checks()

    parser = argparse.ArgumentParser(
        prog="bazel run //:lint --",
        description="Runs the checks of the workspace that a Bazel build doesn't run.",
        epilog="The checks:\n" + "\n".join(f"  {name}" for name in commands),
        # Keeps the epilog's line breaks.
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "checks",
        nargs="*",
        metavar="check",
        help="the checks to run, all of them by default",
    )
    arguments = parser.parse_args()

    unknown = [name for name in arguments.checks if name not in commands]
    if unknown:
        parser.error(f"unknown checks {unknown}, choose from {list(commands)}")

    # `pnpm-install` runs whenever a check that needs it runs. It comes first, so that it's in
    # `started` before any check that waits for it starts.
    names = [name for name in arguments.checks or commands if name != "pnpm-install"]
    if "pnpm-install" in arguments.checks or frontend_checks.intersection(names):
        names.insert(0, "pnpm-install")
    started = {}

    def check(name):
        """Runs a check, and returns its name, whether it passed, and what it printed."""
        if name in frontend_checks:
            _, installed, _ = started["pnpm-install"].result()
            if not installed:
                return name, False, "It didn't run, because pnpm-install failed.\n"
        with tempfile.TemporaryFile("w+", encoding="utf-8", errors="replace") as output:
            passed = run(commands[name], output)
            output.seek(0)
            return name, passed, output.read()

    failed = []
    with ThreadPoolExecutor() as pool:
        for name in names:
            started[name] = pool.submit(check, name)
        for ended in as_completed(started.values()):
            name, passed, output = ended.result()
            print(f"=== {name}\n{output}", end="", flush=True)
            if not passed:
                failed.append(name)
    if failed:
        print("\nThese checks failed:")
        for name in sorted(failed):
            print(f"  {name}")
        sys.exit(1)


if __name__ == "__main__":
    main()
