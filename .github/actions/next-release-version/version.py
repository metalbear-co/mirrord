#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["tomlkit==0.15.1"]
# ///
"""Decide the next release version from unreleased changelog fragments.

Backs the `next-release-version` composite action, which the release workflows use to
work out whether there is anything to release and what version it should carry.

`tomlkit` round-trips the manifest, so only the version value is rewritten and the
surrounding formatting, key ordering, and comments survive untouched. `tomllib` can
read the manifest but cannot write it back, and a regex rewrite would depend on the
version key keeping its current position within `[workspace.package]`.
"""

from __future__ import annotations

import argparse
import pathlib
import sys

import tomlkit

# Fragments whose only effect is invisible to users, in towncrier category order of
# increasing significance. Anything outside this set warrants a minor bump.
PATCH_CATEGORIES = frozenset({"internal", "fixed"})

# Fragments recording work that no user can observe. A release made up entirely of
# these is optional, hence `--release-on-internal-only`.
INVISIBLE_CATEGORIES = frozenset({"internal"})


def read_version(manifest: pathlib.Path) -> tuple[tomlkit.TOMLDocument, str]:
    try:
        document = tomlkit.parse(manifest.read_text())
    except OSError as error:
        sys.exit(f"{manifest}: {error.strerror}")
    package = document.get("workspace", {}).get("package")
    if package is None:
        sys.exit(f"{manifest}: no [workspace.package] table")
    version = package.get("version")
    if version is None:
        sys.exit(f"{manifest}: [workspace.package] has no version key")
    return document, str(version)


def write_version(
    manifest: pathlib.Path, document: tomlkit.TOMLDocument, version: str
) -> None:
    document["workspace"]["package"]["version"] = version
    manifest.write_text(tomlkit.dumps(document))


def bumped(current: str, level: str) -> str:
    parts = current.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        sys.exit(f"{current!r} is not a bare major.minor.patch version")
    major, minor, patch = (int(part) for part in parts)
    if level == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def category(fragment: pathlib.Path) -> str:
    """The towncrier category of a `+summary.category.md` fragment.

    Towncrier appends a numeric counter when two fragments share a summary, as in
    `+summary.category.1.md`, so a trailing all-digit segment is a counter rather than
    the category.
    """
    parts = fragment.stem.split(".")
    if len(parts) > 2 and parts[-1].isdigit():
        parts.pop()
    return parts[-1]


def categories(changelog_dir: pathlib.Path) -> list[str]:
    """Towncrier categories of every unreleased fragment.

    `towncrier build` consumes the fragments when a release is prepared, so whatever
    remains in the directory is by definition unreleased. Only `.md` fragments count,
    which leaves the `changelog_template.jinja` sitting alongside them out of it.
    """
    return sorted(category(path) for path in changelog_dir.glob("*.md"))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=pathlib.Path,
        default=pathlib.Path("Cargo.toml"),
        help="workspace manifest to read (default: %(default)s)",
    )
    parser.add_argument("--changelog-dir", type=pathlib.Path, required=True)
    parser.add_argument(
        "--release-on-internal-only",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="whether fragments that are all internal still warrant a release",
    )

    args = parser.parse_args()
    document, current = read_version(args.manifest)

    if not args.changelog_dir.is_dir():
        sys.exit(f"{args.changelog_dir}: not a directory")

    found = categories(args.changelog_dir)
    visible = [category for category in found if category not in INVISIBLE_CATEGORIES]

    if not found or (not visible and not args.release_on_internal_only):
        print(f"Nothing to release from {len(found)} unreleased fragment(s).", file=sys.stderr)
        print("should_release=false")
        return

    level = "patch" if all(c in PATCH_CATEGORIES for c in found) else "minor"
    new_version = bumped(current, level)
    write_version(args.manifest, document, new_version)

    print(
        f"Releasing {new_version} ({level}) from fragments: {', '.join(found)}",
        file=sys.stderr,
    )
    print("should_release=true")
    print(f"new_version={new_version}")


if __name__ == "__main__":
    main()
