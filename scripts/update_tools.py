"""Moves the programs of a rules_multitool lockfile, `bazel/multitool.lock.json` by default, to their
latest releases, as `bazel run //scripts:update_tools`.

Each program's downloads name its version in their URLs. A program from a GitHub release moves to
the repository's latest release, kubectl to Kubernetes' latest stable release, and Helm, whose
downloads are on get.helm.sh, to the latest release of its GitHub repository. The program rewrites
the version in each download's URL and in the path of the program in its archive, and downloads each
file to compute its new SHA-256. A program whose new release lacks a download for one of the
platforms stays at its version.

A new release can move the program inside its archive, so build the tools afterwards, as
`bazel run //:lint` does, to check that each `file` still exists.
"""

import argparse
import hashlib
import json
import os
import re
import ssl
import sys
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import HTTPSHandler, Request, build_opener, install_opener, urlopen

import certifi

GITHUB_RELEASE = re.compile(r"https://github\.com/(?P<repository>[^/]+/[^/]+)/releases/download/(?P<tag>[^/]+)/")
KUBERNETES_RELEASE = re.compile(r"https://dl\.k8s\.io/release/(?P<tag>[^/]+)/")
HELM_RELEASE = re.compile(r"https://get\.helm\.sh/helm-(?P<tag>v[^-]+)-")


def latest_github_release(repository):
    """Returns the tag of the latest release of the GitHub repository `repository`, like
    `crate-ci/typos`. `GITHUB_TOKEN` raises GitHub's limit on anonymous requests, if it's set."""
    headers = {"Accept": "application/vnd.github+json"}
    if "GITHUB_TOKEN" in os.environ:
        headers["Authorization"] = f"Bearer {os.environ['GITHUB_TOKEN']}"
    with urlopen(Request(f"https://api.github.com/repos/{repository}/releases/latest", headers=headers)) as response:
        return json.load(response)["tag_name"]


def release_tags(url):
    """Returns the tag of the release that `url` downloads, and the tag of the program's latest
    release."""
    if match := GITHUB_RELEASE.match(url):
        return match["tag"], latest_github_release(match["repository"])
    if match := KUBERNETES_RELEASE.match(url):
        with urlopen("https://dl.k8s.io/release/stable.txt") as response:
            return match["tag"], response.read().decode().strip()
    if match := HELM_RELEASE.match(url):
        return match["tag"], latest_github_release("helm/helm")
    sys.exit(f"{url} isn't a GitHub release, Kubernetes or Helm download, whose latest version this program finds.")


def moved(text, current, latest):
    """Returns `text`, a download's URL or a path in its archive, with the version of the release
    `current` replaced by that of `latest`. A tag like `v3.26.0` names the version as `3.26.0` in some
    file names, like Vale's `vale_3.26.0_Linux_64-bit.tar.gz`."""
    return text.replace(current, latest).replace(current.removeprefix("v"), latest.removeprefix("v"))


def sha256(url):
    digest = hashlib.sha256()
    with urlopen(url) as response:
        while chunk := response.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //scripts:update_tools --",
        description="Moves the programs of a rules_multitool lockfile to their latest releases.",
    )
    parser.add_argument(
        "--lockfile",
        default="bazel/multitool.lock.json",
        help="the lockfile, relative to the workspace's root (default: %(default)s)",
    )
    arguments = parser.parse_args()

    if "BUILD_WORKSPACE_DIRECTORY" not in os.environ:
        sys.exit("Run it with `bazel run`.")
    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])

    # The Python interpreter that Bazel downloads doesn't find the machine's certificates on every
    # system, like NixOS, so every request checks the server's certificate against certifi's.
    install_opener(build_opener(HTTPSHandler(context=ssl.create_default_context(cafile=certifi.where()))))

    lockfile = Path(arguments.lockfile)
    programs = json.loads(lockfile.read_text())
    for name, program in programs.items():
        # Like `$schema`.
        if name.startswith("$"):
            continue
        current, latest = release_tags(program["binaries"][0]["url"])
        if current == latest:
            continue

        binaries = []
        for binary in program["binaries"]:
            url = moved(binary["url"], current, latest)
            try:
                digest = sha256(url)
            except HTTPError as error:
                # A release can drop a platform, like cargo-deny 0.22.0 did x86-64 macOS.
                print(f"{name} stays at {current}, because {url} gave {error.code}.", flush=True)
                break
            binaries.append(binary | {"url": url, "sha256": digest})
            if "file" in binary:
                binaries[-1]["file"] = moved(binary["file"], current, latest)
        else:
            print(f"{name} moved from {current} to {latest}.", flush=True)
            program["binaries"] = binaries

    lockfile.write_text(json.dumps(programs, indent=2) + "\n")


if __name__ == "__main__":
    main()
