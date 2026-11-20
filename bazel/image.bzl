"""`image_targets`, which defines an image with the targets that load and push it.

mirrord's images and those of the modules that depend on mirrord, like the operator, use it. The
file also has the labels and the platforms of mirrord's images, and the version of the agent.

Each published image has an `<image>_tag` flag with the `repository:tag` that its `<image>_load`
gives the image, so that devs can load an image under their own name without editing a BUILD file,
like `bazel run //mirrord/agent:image_load --//mirrord/agent:image_tag=you/agent:dev`.

Changing the tag doesn't rebuild the image, because only `<image>_load` reads the flag. The flag is
part of Bazel's configuration, though, so a command with another tag than the command before it
discards Bazel's analysis cache and analyzes its targets again.
"""

load("@bazel_skylib//rules:common_settings.bzl", "string_flag")
load("@cargo_packages//:packages.bzl", "PACKAGES")
load("@rules_img//img:image.bzl", "image_index", "image_manifest")
load("@rules_img//img:load.bzl", "image_load")
load("@rules_img//img:push.bzl", "image_push")
load(":compatibility.bzl", "LINUX_ONLY")
load(":platform.bzl", "platform_label")

# The version of the agent, which tags the agent's image that a release publishes, and which the
# mirrord CLI of that release starts by default. Public for the modules that name it, like the
# operator, whose image says which agent the operator starts by default.
AGENT_VERSION = PACKAGES["mirrord/agent"].version

# GitHub's registry links an image's package to the repository by this label.
IMAGE_LABELS = {"org.opencontainers.image.source": "https://github.com/metalbear-co/mirrord"}

# The platforms that an `image_index` builds its image for. The image for the machine's own CPU
# shares the top-level build's outputs (see `platform_label`).
IMAGE_PLATFORMS = [
    platform_label("linux", cpu)
    for cpu in [
        "aarch64",
        "x86_64",
    ]
]

def image_targets(
        name,
        registry,
        repository,
        manifest = None,
        platforms = IMAGE_PLATFORMS,
        manual = False,
        load_visibility = None,
        **kwargs):
    """Defines the image `name`, and the targets that load it into Docker and push it.

    The targets are:

    - `<name>`, the image for the build's own platform: an `image_manifest` with the attributes of
      `kwargs`, or with `manifest`, an alias of that image.
    - `<name>_load`, which loads the image for the build's own platform into Docker, tagged with
      the value of the `<name>_tag` flag, `<registry>/<repository>:latest` by default.
    - `<name>_index`, the image for each of `platforms`, and `<name>_index.digest`, the file with
      its digest, `sha256:<hex>`, which a release attests. A workflow finds the file with
      `bazel cquery --output=files //<package>:<name>_index.digest`.
    - `<name>_push`, which pushes the index to `<registry>/<repository>`, with each tag that the
      command passes: `bazel run -c opt //<package>:<name>_push -- --tag <tag>`. `--registry` and
      `--repository` push it somewhere else.

    The index and the push are `manual`, because they build the image for every platform.

    Args:
      name: The name of the image, which each of the other targets starts with.
      registry: The registry that the image is published in, like `ghcr.io`.
      repository: The image's repository in the registry, like `metalbear-co/mirrord`.
      manifest: An image that another target defines, like an `image_variant` of an image, or one
        that two `image_targets` publish to two repositories. Without it, the macro defines the
        image from `kwargs`.
      platforms: The platforms that the index has the image for, both Linux CPUs by default.
      manual: Whether `<name>` and `<name>_load` are `manual` too, for an image that `//...`
        doesn't build.
      load_visibility: The visibility of `<name>_load`, for an image that the end-to-end suites of
        other packages load into their clusters.
      **kwargs: The attributes of rules_img's `image_manifest`, like `layers`. Images run on Linux,
        so `target_compatible_with` is Linux by default.
    """
    tags = ["manual"] if manual else []
    if manifest:
        if kwargs:
            fail("`manifest` names an image that another target defines, so the image takes " +
                 "no attributes of its own, like {}.".format(", ".join(kwargs)))
        native.alias(
            name = name,
            actual = manifest,
            tags = tags,
        )
    else:
        kwargs.setdefault("target_compatible_with", LINUX_ONLY)
        image_manifest(
            name = name,
            tags = kwargs.pop("tags", []) + tags,
            **kwargs
        )

    string_flag(
        name = name + "_tag",
        build_setting_default = "{}/{}:latest".format(registry, repository),
    )

    image_load(
        name = name + "_load",
        build_settings = {"IMAGE_TAG": ":{}_tag".format(name)},
        image = ":" + name,
        tag = "{{.IMAGE_TAG}}",
        tags = tags,
        target_compatible_with = LINUX_ONLY,
        visibility = load_visibility,
    )

    image_index(
        name = name + "_index",
        manifests = [":" + name],
        platforms = platforms,
        tags = ["manual"],
    )

    native.filegroup(
        name = name + "_index.digest",
        srcs = [":{}_index".format(name)],
        output_group = "digest",
        tags = ["manual"],
    )

    image_push(
        name = name + "_push",
        registry = registry,
        repository = repository,
        image = ":{}_index".format(name),
        tags = ["manual"],
    )
