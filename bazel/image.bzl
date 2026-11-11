"""What the workspace's images share: their labels, and a flag for the tag that `oci_load` gives one.

The tag is a flag rather than an attribute, so that people can load an image under their own
repository, like `bazel run //mirrord/agent:image_load --//mirrord/agent:image_tag=you/agent:dev`,
without editing a BUILD file.

The flag's target is also the file of tags that `oci_load`'s `repo_tags` takes, so only the actions
that load the image depend on the flag: changing it doesn't build the image or anything in it again.
"""

# The labels of every image that the workspace publishes, for `oci_image`'s `labels`. GitHub's
# registry reads `org.opencontainers.image.source` to connect an image's package to the repository,
# which shows the package on the repository's page and lets it take the repository's permissions.
IMAGE_LABELS = {"org.opencontainers.image.source": "https://github.com/metalbear-co/mirrord"}

def _image_tag_impl(ctx):
    tags = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(tags, ctx.build_setting_value + "\n")
    return [DefaultInfo(files = depset([tags]))]

image_tag = rule(
    implementation = _image_tag_impl,
    build_setting = config.string(flag = True),
    doc = "A `repository:tag` for an image, and a file with it, for `oci_load`'s `repo_tags`.",
)
