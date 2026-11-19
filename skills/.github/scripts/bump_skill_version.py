#!/usr/bin/env python3
"""Bump a skill's minor version for an auto-update PR and print the new version.

Usage:
    python3 .github/scripts/bump_skill_version.py <skill> <branch-prefix>

Example:
    python3 .github/scripts/bump_skill_version.py mirrord-up auto/update-mirrord-up-refs-v

The version on main only advances when a PR merges, so a naive +1 reproduces
the same number every week while a prior auto-PR is still open, colliding on
the branch name. This collects the versions already claimed by open
<branch-prefix><version> branches on origin and picks the next free minor above
all of them, then writes it into skills/<skill>/SKILL.md.

Stdlib only. Run from the repository root.
"""

import pathlib
import re
import subprocess
import sys


def main():
    skill, prefix = sys.argv[1:3]
    heads = subprocess.run(
        ["git", "ls-remote", "--heads", "origin", f"refs/heads/{prefix}*"],
        check=True, capture_output=True, text=True,
    ).stdout

    p = pathlib.Path(f"skills/{skill}/SKILL.md")
    text = p.read_text()
    major, minor = (int(x) for x in re.search(r'version: "([\d.]+)"', text).group(1).split("."))

    taken = set()
    for line in heads.splitlines():
        parts = line.split(f"refs/heads/{prefix}", 1)[-1].split(".")
        if len(parts) == 2 and parts[0] == str(major) and parts[1].isdigit():
            taken.add(int(parts[1]))

    new_minor = minor + 1
    while new_minor in taken:
        new_minor += 1

    new_version = f"{major}.{new_minor}"
    text = re.sub(r'version: "([\d.]+)"', f'version: "{new_version}"', text, count=1)
    p.write_text(text)
    print(new_version)


if __name__ == "__main__":
    main()
