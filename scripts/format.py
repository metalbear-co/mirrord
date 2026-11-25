"""Formats the workspace's code, as `bazel run //:format`, so that devs and CI run every formatter
with one command. This file lists mirrord's formatters, and `run_format` in `mirrord_runner.py` runs them.
"""

import os

from mirrord_runner import Formatter, Setup, run_format
from mirrord_tools import PNPM_INSTALL, rlocation

from scripts.frontends import FRONTENDS

rustfmt = rlocation(os.environ["RUSTFMT"])

formatters = {
    # rules_rust's runner formats each target's sources with the target's edition, as
    # `cargo fmt` does with each crate's.
    "rustfmt": Formatter("the Rust code", [rustfmt], [rustfmt, "--", "--check"]),
    "buildifier": Formatter(
        "the Starlark files, fixing what buildifier's lint can",
        ["buildifier", "-r", "-lint=fix", "."],
        ["buildifier", "-r", "-mode=check", "-lint=warn", "."],
    ),
    "ruff": Formatter("the Python code", ["ruff", "format"], ["ruff", "format", "--check"]),
}

# The frontends take Prettier from their packages.
pnpm_install = Setup("pnpm install", PNPM_INSTALL)
for package in FRONTENDS:
    formatters[f"prettier-{package}"] = Formatter(
        f"Prettier on {package}",
        ["pnpm", "--filter", package, "run", "format"],
        ["pnpm", "--filter", package, "run", "format:check"],
        setup=pnpm_install,
    )

run_format(formatters)
