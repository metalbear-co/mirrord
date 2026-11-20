"""`cargo_packages`, which reads the `CARGO_PKG_*` metadata and the targets of each workspace member.

mirrord relies on the `CARGO_PKG_*` variables that Cargo sets from each package's `Cargo.toml`:
the CLI's `--version` and `--help`, the agent image's default tag, and the protocol version that
`mirrord-protocol` negotiates all come from them.

rules_rust doesn't read `Cargo.toml`, so it sets only `CARGO_PKG_NAME` and `CARGO_PKG_VERSION`,
from the target's name and `version` attribute, and leaves the rest empty. The `@crates` hub that
rules_rs generates doesn't have the metadata of the workspace's own crates either, so the
workspace's Rust macros take it from `@cargo_packages`.

nextest reads the workspace's packages from the output of `cargo metadata`, which the rule writes
too, as `metadata.json`. nextest only accepts a `package()`, `binary()` or `binary_id()` filter that
matches a package or a target of the workspace, like the ones in `.config/nextest.toml` in a test of
another package, so the file lists every member with its Cargo targets. It only has the fields that
nextest reads. Every test depends on the file, so every test runs again when it changes: when a
package or a Cargo target, like a file in `tests/`, is added, removed or renamed, or when a
package's version changes.

It's a repository rule rather than a build action because BUILD files load its output, and a BUILD
file can only load a file that exists before Bazel runs any action.
"""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("@toml.bzl", "toml")

# Where Cargo looks for the targets of each kind that the manifest doesn't declare: `<name>.rs` and
# `<name>/main.rs`.
_TARGET_DIRECTORIES = {
    "bin": "src/bin",
    "example": "examples",
    "test": "tests",
}

# The workspace's root in `metadata.json`. The real root is a path that differs between machines, so
# this fixed one keeps the file the same everywhere, which lets the remote cache share the test
# results between machines. Each test maps it to its runfiles with nextest's `--workspace-remap`.
_WORKSPACE_ROOT = "/bazel-nextest-workspace"

_FIELDS = {
    "CARGO_PKG_AUTHORS": "authors",
    "CARGO_PKG_DESCRIPTION": "description",
    "CARGO_PKG_HOMEPAGE": "homepage",
    "CARGO_PKG_LICENSE": "license",
    "CARGO_PKG_NAME": "name",
    "CARGO_PKG_REPOSITORY": "repository",
}

def _member_directories(rctx, root, patterns, excluded):
    directories = []
    for pattern in patterns:
        if "*" not in pattern:
            directories.append(pattern)
            continue

        parent, _, last = pattern.rpartition("/")
        if last != "*" or "*" in parent:
            fail("`{}`: only `*` as the whole last component is supported".format(pattern))
        for entry in rctx.path(root + "/" + parent).readdir():
            # Checking `exists` isn't a read, so without `watch`, Bazel wouldn't rerun the rule when
            # a manifest is added or removed.
            manifest = entry.get_child("Cargo.toml")
            rctx.watch(manifest)
            if manifest.exists:
                directories.append(parent + "/" + entry.basename)

    return [directory for directory in directories if directory not in excluded]

def _exists(rctx, path):
    # Checking `exists` isn't a read, so without `watch`, Bazel wouldn't rerun the rule when the
    # file is added or removed.
    rctx.watch(path)
    return path.exists

def _targets(rctx, package_root, manifest):
    """Returns the package's Cargo targets by kind, each a dict from its name to its file.

    Cargo takes the targets from the manifest and from the package's files
    (https://doc.rust-lang.org/cargo/reference/cargo-targets.html#target-auto-discovery). A file
    only adds a target when the manifest declares none with its name or its path.

    The `autobins`, `autoexamples` and `autotests` fields, which turn off looking for the files,
    aren't supported.
    """
    name = manifest["package"]["name"]

    # Every kind of library is a `lib` to nextest.
    targets = {"lib": {}}
    library = manifest.get("lib", {})
    library_path = library.get("path", "src/lib.rs")
    if _exists(rctx, package_root.get_child(library_path)):
        targets["lib"][library.get("name", name.replace("-", "_"))] = library_path

    for kind, directory in _TARGET_DIRECTORIES.items():
        inferred = {}
        if kind == "bin" and _exists(rctx, package_root.get_child("src/main.rs")):
            inferred[name] = "src/main.rs"

        directory_path = package_root.get_child(directory)
        entries = []
        if _exists(rctx, directory_path):
            entries = directory_path.readdir()
        for entry in entries:
            if entry.is_dir and _exists(rctx, entry.get_child("main.rs")):
                inferred[entry.basename] = "{}/{}/main.rs".format(directory, entry.basename)
            elif not entry.is_dir and entry.basename.endswith(".rs"):
                inferred[entry.basename.removesuffix(".rs")] = directory + "/" + entry.basename

        # A declared target without a `path` is in the file that Cargo would infer for its name.
        declared = {}
        for target in manifest.get(kind, []):
            target_name = target["name"]
            declared[target_name] = (
                target.get("path") or
                inferred.get(target_name) or
                "{}/{}.rs".format(directory, target_name)
            )
        targets[kind] = declared | {
            inferred_name: path
            for inferred_name, path in sorted(inferred.items())
            if inferred_name not in declared and path not in declared.values()
        }

    return targets

def _field(package, workspace_package, field):
    value = package.get(field)
    if type(value) == "dict" and value.get("workspace"):
        return workspace_package.get(field)
    return value

def _optional_dependencies(directory, manifest, workspace_dependencies):
    """Returns the package's optional dependencies, keyed by the names that `[features]` uses.

    Each value is how the `@crates` hub's crate data names the dependency: `//<directory>` for a
    path dependency, like `//mirrord/kube`, and the package's name otherwise, which starts the hub's
    label, as in `@crates//tokio-1.54.1`.
    """
    tables = [manifest.get("dependencies", {})] + [
        target.get("dependencies", {})
        for target in manifest.get("target", {}).values()
    ]
    dependencies = {}
    for table in tables:
        for name, dependency in table.items():
            if type(dependency) != "dict" or not dependency.get("optional"):
                continue

            # The path of a dependency from `[workspace.dependencies]` is relative to the workspace's
            # root, and the package's own is relative to the package.
            base = directory
            if dependency.get("workspace"):
                inherited = workspace_dependencies[name]
                if type(inherited) == "dict":
                    dependency = inherited | dependency
                    base = ""

            if "path" in dependency:
                dependencies[name] = "//" + paths.normalize(paths.join(base, dependency["path"]))
            else:
                dependencies[name] = dependency.get("package", name)
    return dependencies

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
        manifest = toml.decode(rctx.read(root + "/" + directory + "/Cargo.toml"))
        package = manifest["package"]
        rustc_env = {}
        for variable, field in _FIELDS.items():
            value = _field(package, workspace_package, field) or ""
            rustc_env[variable] = ":".join(value) if type(value) == "list" else value
        version = _field(package, workspace_package, "version") or "0.0.0"
        packages[directory] = struct(
            version = version,
            doctest = manifest.get("lib", {}).get("doctest", True),
            features = manifest.get("features", {}),
            optional_dependencies = _optional_dependencies(
                directory,
                manifest,
                workspace.get("dependencies", {}),
            ),
            # nextest treats the ID as an opaque string, so it only has to be unique, and to match
            # between `metadata.json` and the list of test binaries that the tests' runner writes.
            # It has the format of `cargo metadata`.
            package_id = "path+file://{}/{}#{}@{}".format(_WORKSPACE_ROOT, directory, package["name"], version),
            rustc_env = rustc_env,
            targets = _targets(rctx, rctx.path(root + "/" + directory), manifest),
            untested_binaries = [
                target["name"]
                for target in manifest.get("bin", [])
                if not target.get("test", True)
            ],
        )

    metadata_packages = []
    for directory, package in packages.items():
        manifest_path = "{}/{}/Cargo.toml".format(_WORKSPACE_ROOT, directory)
        metadata_packages.append({
            "name": package.rustc_env["CARGO_PKG_NAME"],
            "version": package.version,
            "id": package.package_id,
            "manifest_path": manifest_path,
            # nextest only reads the targets to check that each `binary_id()` and `binary()` filter
            # matches one, by its kind and its name. Its parser requires the other fields.
            "targets": [
                {
                    "kind": [kind],
                    "name": name,
                    "crate_types": ["lib" if kind == "lib" else "bin"],
                    "src_path": "{}/{}/{}".format(_WORKSPACE_ROOT, directory, path),
                }
                for kind, targets in package.targets.items()
                for name, path in targets.items()
            ],
            # nextest's parser requires these fields.
            "dependencies": [],
            "features": {},
        })

    rctx.file("BUILD.bazel", 'exports_files(["metadata.json"], visibility = ["//visibility:public"])\n')
    rctx.file("packages.bzl", "PACKAGES = {}\n".format(repr(packages)))
    rctx.file("metadata.json", json.encode({
        "version": 1,
        "workspace_root": _WORKSPACE_ROOT,
        # nextest takes the target directory from the list of test binaries instead.
        "target_directory": _WORKSPACE_ROOT + "/target",
        "workspace_members": [package["id"] for package in metadata_packages],
        "resolve": None,
        "packages": metadata_packages,
    }))
    return rctx.repo_metadata(reproducible = True)

cargo_packages = repository_rule(
    implementation = _cargo_packages_impl,
    doc = """Writes `PACKAGES` to `packages.bzl`, and the workspace's packages in the format of
    `cargo metadata --no-deps` to `metadata.json`.

    `PACKAGES` maps each workspace member's directory, like `mirrord/cli`, to its `version`, to
    `rustc_env`, its other `CARGO_PKG_*` variables, to `targets`, its Cargo targets by kind (`lib`,
    `bin`, `example` and `test`), each a dict from the target's name to its file, like
    `tests/<name>.rs`, to `untested_binaries`, the binaries whose `[[bin]]` sets `test = false`,
    whose unit tests `cargo test` skips, to `doctest`, whether `cargo test` runs the library's
    doctests, to `features`, its `[features]` table, to `optional_dependencies`, which maps the name
    of each optional dependency in `[features]` to its crate (see `_optional_dependencies`), and to
    `package_id`, its ID in `metadata.json`.

    A field with `workspace = true` takes the value from `[workspace.package]`, as in Cargo.

    A member without a version gets `0.0.0`.""",
    attrs = {
        "cargo_toml": attr.label(
            doc = "The workspace's root `Cargo.toml`.",
            allow_single_file = True,
            mandatory = True,
        ),
    },
)
