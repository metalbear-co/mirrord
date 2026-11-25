"""Runs the checks that a Bazel build doesn't run, as `bazel run //:lint`: the linters of the docs,
the Python code, the Starlark code and the frontends, the checks of the changelog fragment and the
dependencies, and the checks that `docs/` and `skills/` ran in their own repositories.

It runs the tools that Bazel downloads or builds (see `//scripts/tools`), so CI runs the same versions
as devs. This file lists mirrord's checks, and `run_lint` in `mirrord_runner.py` runs them.
"""

import os
import sys

from mirrord_runner import Check, Setup, git_files, run_lint, starpls_check, ty_command
from mirrord_tools import PNPM_INSTALL, runfile_program

from scripts.frontends import FRONTENDS

checks = {
    # typos never ran on `docs/`, `guides/` and `skills/` in their own repositories, and finds both
    # real typos and identifiers such as `aks` there, so they stay out until those are fixed or
    # added to `typos.toml`.
    "typos": Check(
        "spelling in the Markdown files and in mirrord-config's docs",
        [
            "typos",
            "--format",
            "brief",
            *git_files("*.md", ":!docs", ":!guides", ":!skills", "mirrord/config/*.rs"),
        ],
    ),
    # Vale finds `report.tmpl` in `.vale/styles/config/templates`.
    "vale": Check(
        "the style of the Markdown files and of the changelog fragments",
        [
            "vale",
            "--output=report.tmpl",
            *git_files(":(glob)*.md", ":!CHANGELOG.md", "changelog.d/*.md"),
        ],
    ),
    "markdownlint": Check("the Markdown of README.md", ["markdownlint-cli2", "README.md"]),
    "towncrier": Check("that the branch adds a changelog fragment, compared with origin/main", ["towncrier", "check"]),
    "cargo-deny": Check(
        "the dependencies' licenses, sources and security advisories",
        ["cargo", "deny", "--all-features", "check"],
    ),
    "cargo-shear": Check("unused dependencies of the crates", ["cargo", "shear", "--deny-warnings"]),
    "ruff": Check("Ruff's lints of the Python code", ["ruff", "check"]),
    # buildifier only formats and lints the Starlark code, and starpls also checks its types.
    "starpls": starpls_check(),
    "ty": Check("the types of the Python code", ty_command()),
    # `mirrord mcp` reads the plan tags to tell users which plan a feature needs.
    "docs-plan-tags": Check(
        "that every docs feature page has a plan tag",
        [sys.executable, "docs/.github/scripts/check_plan_tags.py"],
    ),
    # The check validates against the schemas of the checkout rather than the copies in `skills/`,
    # so a schema change that breaks a skill fails on its own pull request.
    "skills-fence-tags": Check(
        "that every JSON and YAML block of the skills is tagged, and that their configs validate",
        [
            runfile_program("skills/check_fence_tags"),
            "--mirrord-schema",
            "mirrord-schema.json",
            "--mirrord-up-schema",
            "mirrord-up-schema.json",
        ],
    ),
}

# The tests fake `gh` and `curl` with shell scripts, which don't run on Windows.
if os.name != "nt":
    checks["flaky-report-tests"] = Check(
        "the tests of .github/scripts/flaky-tests.sh",
        [sys.executable, ".github/scripts/test_flaky_report.py"],
    )

# The frontends take ESLint and TypeScript from their packages.
pnpm_install = Setup("pnpm install", PNPM_INSTALL)
for package in FRONTENDS:
    checks[f"eslint-{package}"] = Check(
        f"ESLint on {package}",
        ["pnpm", "--filter", package, "run", "lint"],
        setup=pnpm_install,
    )
    checks[f"typecheck-{package}"] = Check(
        f"TypeScript's type check of {package}",
        ["pnpm", "--filter", package, "run", "typecheck"],
        setup=pnpm_install,
    )

run_lint(checks)
