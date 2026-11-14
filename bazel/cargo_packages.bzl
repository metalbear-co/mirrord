"""Makes the package metadata from the Cargo workspace's manifests available to BUILD files.

Cargo sets `CARGO_PKG_NAME`, `CARGO_PKG_VERSION`, `CARGO_PKG_DESCRIPTION` and the other
`CARGO_PKG_*` variables from each package's manifest. mirrord uses them as the CLI's name in
`mirrord --version`, to pick the agent's image tag, as the protocol version that `mirrord-protocol`
negotiates, and in `--help`. rules_rust sets only `CARGO_PKG_NAME`, to the target's name, and
`CARGO_PKG_VERSION`, to the `version` attribute, which defaults to `0.0.0`. It leaves the other
variables empty, and the `@crates` hub doesn't provide the metadata of the workspace's own crates.
So this repository rule reads it from the manifests.

The rule writes `PACKAGES` to `@cargo_packages//:packages.bzl`. It maps each workspace member's
directory, like `mirrord/cli`, to a struct with:

- `version`: the member's version, which `//bazel:rust.bzl` gives the member's Rust targets, and
  `0.0.0` for a member without one, as with Cargo.
- `rustc_env`: the other `CARGO_PKG_*` variables that Cargo sets from the manifest, which
  `//bazel:rust.bzl` passes to rustc.

A field with `workspace = true`, like `version.workspace = true`, takes the value from
`[workspace.package]` of the root manifest, as with Cargo. The members are the directories that
`members` lists, without those in `exclude`. A member pattern can only use `*` as its whole last
component, like `mirrord/*`.

This has to be a repository rule rather than a build action, because a BUILD file can only load a
file that exists while Bazel reads the BUILD file. Bazel runs a repository rule as soon as a BUILD
file loads from its repository, while a build action only runs after Bazel has read the BUILD
files. Bazel runs the rule again when a manifest that it read changes, or when a directory that a
member pattern lists gains or loses an entry or a manifest.
"""

load("@toml.bzl", "toml")

# The manifest fields that Cargo passes to rustc, by the variable that it passes each one in. Cargo
# joins the authors with `:`.
_FIELDS = {
    "CARGO_PKG_AUTHORS": "authors",
    "CARGO_PKG_DESCRIPTION": "description",
    "CARGO_PKG_HOMEPAGE": "homepage",
    "CARGO_PKG_LICENSE": "license",
    "CARGO_PKG_NAME": "name",
    "CARGO_PKG_REPOSITORY": "repository",
}

def _member_directories(rctx, root, patterns, excluded):
    """Returns the member directories that `patterns`, Cargo's `members`, name under `root`."""
    directories = []
    for pattern in patterns:
        if "*" not in pattern:
            directories.append(pattern)
            continue

        parent, _, last = pattern.rpartition("/")
        if last != "*" or "*" in parent:
            fail("`{}`: only `*` as the whole last component is supported".format(pattern))
        for entry in rctx.path(root + "/" + parent).readdir():
            # `exists` doesn't make Bazel run the rule again when the file appears or goes, as
            # reading it would, so the rule watches it.
            manifest = entry.get_child("Cargo.toml")
            rctx.watch(manifest)
            if manifest.exists:
                directories.append(parent + "/" + entry.basename)

    return [directory for directory in directories if directory not in excluded]

def _field(package, workspace_package, field):
    """Returns `field` of `package`, a `[package]` table, taken from the workspace if it says so."""
    value = package.get(field)
    if type(value) == "dict" and value.get("workspace"):
        return workspace_package.get(field)
    return value

def _cargo_packages_impl(rctx):
    root_manifest = rctx.path(rctx.attr.cargo_toml)
    root = str(root_manifest.dirname)
    workspace = toml.decode(rctx.read(root_manifest))["workspace"]
    workspace_package = workspace.get("package", {})

    packages = {}
    for directory in _member_directories(
        rctx,
        root,
        workspace.get("members", []),
        workspace.get("exclude", []),
    ):
        package = toml.decode(rctx.read(root + "/" + directory + "/Cargo.toml"))["package"]
        rustc_env = {}
        for variable, field in _FIELDS.items():
            value = _field(package, workspace_package, field) or ""
            rustc_env[variable] = ":".join(value) if type(value) == "list" else value
        packages[directory] = struct(
            version = _field(package, workspace_package, "version") or "0.0.0",
            rustc_env = rustc_env,
        )

    rctx.file("BUILD.bazel", "")
    rctx.file("packages.bzl", "PACKAGES = {}\n".format(repr(packages)))
    return rctx.repo_metadata(reproducible = True)

cargo_packages = repository_rule(
    implementation = _cargo_packages_impl,
    attrs = {
        "cargo_toml": attr.label(allow_single_file = True, mandatory = True),
    },
)
