"""Formats the workspace's Rust code with rustfmt, its Starlark files with buildifier, and the
frontends' code with Prettier. With `--check`, it changes nothing and fails if a file isn't
formatted. It runs as `bazel run //:format`, which runs it in the workspace.

rustfmt is the one of the Rust toolchain that Bazel builds with. rules_rust's runner, which queries
Bazel for the Rust targets, formats each target's sources with the target's edition, as `cargo fmt`
formats each crate with the edition in its `Cargo.toml`. buildifier comes from `PATH`, and also
applies its lint fixes, or reports its lint warnings with `--check`. Prettier comes from the
frontends' `node_modules`, which pnpm, from `PATH`, installs.
"""

import argparse
import os
import sys

from python.runfiles import Runfiles

from scripts.tools import FRONTENDS, PNPM_INSTALL, run


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //:format --",
        description="Formats the Rust code, the Starlark files and the frontends.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="change nothing, and fail if a file isn't formatted",
    )
    # Bazel passes rules_rust's rustfmt runner as a runfiles path.
    parser.add_argument("--rustfmt", required=True, help=argparse.SUPPRESS)
    arguments = parser.parse_args()

    rustfmt = Runfiles.Create().Rlocation(arguments.rustfmt)
    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])

    failed = []

    print("=== rustfmt", flush=True)
    if not run([rustfmt, "--", "--check"] if arguments.check else [rustfmt]):
        failed.append("rustfmt")

    print("=== buildifier", flush=True)
    if arguments.check:
        buildifier = ["buildifier", "-r", "-mode=check", "-lint=warn", "."]
    else:
        buildifier = ["buildifier", "-r", "-lint=fix", "."]
    if not run(buildifier):
        failed.append("buildifier")

    print("=== prettier", flush=True)
    if not run(PNPM_INSTALL):
        failed.append("prettier")
    else:
        script = "format:check" if arguments.check else "format"
        for package in FRONTENDS:
            if not run(["pnpm", "--filter", package, "run", script]):
                failed.append(f"prettier-{package}")

    if failed:
        print("\nThese formatters failed:")
        for name in failed:
            print(f"  {name}")
        sys.exit(1)


if __name__ == "__main__":
    main()
