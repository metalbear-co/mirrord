"""Moves the pins of what the images are built from to the latest versions, as
`bazel run //scripts:update_images`.

`MODULE.bazel` pins the inputs of the images, so that every build gets the same files:

- each `pull` names its base image by `digest`, and this resolves the digest again from the image's
  `tag`, through the registry's API;
- each `apt.sources_list` names a snapshot of Debian's archive by its time, and this moves it to the
  archive's latest snapshot.

The images only get the fixes of their base images and of Debian's packages when the pins move, so
`.github/workflows/update-images.yaml` runs this every week and opens a pull request with what
changed. The program ends with `bazel mod deps --lockfile_mode=update`, because `MODULE.bazel.lock`
holds the packages that `apt.install` resolves from the snapshot.

Another module, like the operator, can run it on its own `MODULE.bazel` with
`bazel run @mirrord//scripts:update_images`. To move pins of its own too, its program calls `main`
with functions that take the text of `MODULE.bazel` and return it with their pins moved.
"""

import argparse
import hashlib
import json
import os
import re
import ssl
import sys
import urllib.parse
from collections.abc import Callable, Iterable
from datetime import UTC, datetime
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import HTTPSHandler, Request, build_opener, install_opener, urlopen

import certifi
from mirrord_tools import run

# A `pull` call, whose arguments are in `body`.
PULL = re.compile(r"^(?P<indent>[ \t]*)pull\((?P<body>.*?)^(?P=indent)\)", re.MULTILINE | re.DOTALL)

# The URI of a snapshot of one of the archives of snapshot.debian.org, like `debian` or
# `debian-security`.
SNAPSHOT = re.compile(r"https://snapshot\.debian\.org/archive/(?P<archive>[\w-]+)/(?P<time>\d{8}T\d{6}Z)")

# The registries whose API is at another host than the name that images use.
REGISTRY_HOSTS = {
    "docker.io": "registry-1.docker.io",
    "index.docker.io": "registry-1.docker.io",
}

# The manifests that a tag can name, an index of several platforms' images first, which is what a
# `pull` with `platforms` pins.
MANIFEST_TYPES = (
    "application/vnd.oci.image.index.v1+json, "
    "application/vnd.docker.distribution.manifest.list.v2+json, "
    "application/vnd.oci.image.manifest.v1+json, "
    "application/vnd.docker.distribution.manifest.v2+json"
)


def argument(body, name):
    """Returns the string value of the argument `name` of a call whose arguments are `body`."""
    match = re.search(rf'^\s*{name} = "([^"]*)"', body, re.MULTILINE)
    return match[1] if match else None


def anonymous_token(challenge):
    """Returns a token for an anonymous pull, from the registry's `WWW-Authenticate` challenge."""
    parameters = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
    query = urllib.parse.urlencode({key: parameters[key] for key in ["service", "scope"] if key in parameters})
    with urlopen(f"{parameters['realm']}?{query}") as response:
        token = json.load(response)
    return token.get("token") or token["access_token"]


def image_digest(registry, repository, tag):
    """Returns the digest of the manifest that `tag` names in the registry."""
    host = REGISTRY_HOSTS.get(registry, registry)
    request = Request(f"https://{host}/v2/{repository}/manifests/{tag}", headers={"Accept": MANIFEST_TYPES})
    try:
        response = urlopen(request)
    except HTTPError as error:
        # Registries like Docker Hub want a token even for public images.
        if error.code != 401:
            raise
        request.add_header("Authorization", f"Bearer {anonymous_token(error.headers['WWW-Authenticate'])}")
        response = urlopen(request)
    # The digest is the hash of the manifest's bytes, as the registry stores them.
    with response:
        return "sha256:" + hashlib.sha256(response.read()).hexdigest()


def update_pulls(module):
    """Returns `module` with the `digest` of each `pull` that has a `tag` resolved from that tag."""

    def update(match):
        body = match["body"]
        name, registry, repository, tag, digest = (
            argument(body, key) for key in ["name", "registry", "repository", "tag", "digest"]
        )
        if not (registry and repository and tag and digest):
            return match[0]
        latest = image_digest(registry, repository, tag)
        if latest == digest:
            return match[0]
        print(f"{name}: {registry}/{repository}:{tag} moved from {digest} to {latest}.", flush=True)
        return match[0].replace(f'digest = "{digest}"', f'digest = "{latest}"')

    return PULL.sub(update, module)


def update_snapshots(module):
    """Returns `module` with each snapshot of snapshot.debian.org moved to its archive's latest."""
    latest = {}
    now = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    for archive in {match["archive"] for match in SNAPSHOT.finditer(module)}:
        # snapshot.debian.org redirects a time to the snapshot that was the latest at that time.
        with urlopen(f"https://snapshot.debian.org/archive/{archive}/{now}/") as response:
            snapshot = SNAPSHOT.match(response.geturl())
        if snapshot is None:
            sys.exit(f"snapshot.debian.org redirected the time {now} of {archive} to {response.geturl()}.")
        latest[archive] = snapshot["time"]

    def update(match):
        time = latest[match["archive"]]
        if time != match["time"]:
            print(f"The snapshot of {match['archive']} moved from {match['time']} to {time}.", flush=True)
        return f"https://snapshot.debian.org/archive/{match['archive']}/{time}"

    return SNAPSHOT.sub(update, module)


def main(updates: Iterable[Callable[[str], str]] = ()):
    """Moves the pins of `update_pulls`, `update_snapshots` and `updates` in `MODULE.bazel`, and
    then updates `MODULE.bazel.lock`."""
    if "BUILD_WORKSPACE_DIRECTORY" not in os.environ:
        sys.exit("Run it with `bazel run`.")
    os.chdir(os.environ["BUILD_WORKSPACE_DIRECTORY"])

    # The Python interpreter that Bazel downloads doesn't find the machine's certificates on every
    # system, like NixOS, so every request checks the server's certificate against certifi's.
    install_opener(build_opener(HTTPSHandler(context=ssl.create_default_context(cafile=certifi.where()))))

    module_file = Path("MODULE.bazel")
    module = module_file.read_text()
    for update in [update_pulls, update_snapshots, *updates]:
        module = update(module)
    module_file.write_text(module)

    if not run(["bazel", "mod", "deps", "--lockfile_mode=update"]):
        sys.exit("`bazel mod deps` failed, so MODULE.bazel.lock may not match MODULE.bazel.")


if __name__ == "__main__":
    # The parser only gives `--help`, but every script takes it, and without a parser the update
    # would run instead. Another module's program parses its own arguments before it calls `main`.
    argparse.ArgumentParser(
        prog="bazel run //scripts:update_images --",
        description="Moves the digests of the base images and the Debian snapshots in MODULE.bazel to "
        "their latest versions, and updates MODULE.bazel.lock.",
    ).parse_args()
    main()
