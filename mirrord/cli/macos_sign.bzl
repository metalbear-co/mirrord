"""`macos_sign`, which signs the Mac files that a release distributes, the universal CLI and layer.

A release signs them with MetalBear's Developer ID, which it gives in
`--//mirrord/cli:macos_signing_p12_file` and `--//mirrord/cli:macos_signing_p12_password_file`.
Without the flags, it signs ad hoc, without a certificate. An ad hoc signature only depends on the
file, so Bazel caches the action like any other.

Every macOS build signs `//mirrord/layer:universal_layer`, which the CLI embeds, so the tests load
a layer with the same kind of signature as the released one, only ad hoc rather than with the
Developer ID. Only the `manual` `//mirrord/cli:cli_universal_signed` signs the universal CLI itself.

The files don't need this signature to load: lld signs arm64 code ad hoc, `llvm-objcopy` signs the
layer's arm64e shim ad hoc again after `//mirrord/layer:shim_arm64e` edits its header, and x86-64
Macs run unsigned code.

It signs with `rcodesign` rather than Apple's `codesign`, because `rcodesign` runs on Linux too and
reads the signing identity from a file rather than from a keychain. So the release builds and signs
the Mac files on a Linux runner.
"""

load("@bazel_skylib//rules:common_settings.bzl", "BuildSettingInfo")

_CODESIGN_TOOLCHAIN = "@codesign.bzl//toolchain:toolchain_type"

def _macos_sign_impl(ctx):
    p12_file = ctx.attr._p12_file[BuildSettingInfo].value
    p12_password_file = ctx.attr._p12_password_file[BuildSettingInfo].value
    if bool(p12_file) != bool(p12_password_file):
        fail("--//mirrord/cli:macos_signing_p12_file and " +
             "--//mirrord/cli:macos_signing_p12_password_file must be set together")

    args = ctx.actions.args()
    args.add("sign")
    args.add("--binary-identifier", ctx.attr.identifier)

    # Notarization requires the hardened runtime.
    args.add("--code-signature-flags", "runtime")

    execution_requirements = {}
    signer = "ad hoc"
    if p12_file:
        signer = "with " + p12_file

        # The flags hold paths rather than the secrets, so that the secrets stay out of the action's
        # inputs and the caches.
        args.add("--p12-file", p12_file)
        args.add("--p12-password-file", p12_password_file)
        execution_requirements = {
            # Bazel can't see the files that the flags name, so the sandbox can hide them (Linux's
            # hides `/tmp`), and the caches would keep a signature after the identity changes.
            "no-cache": "1",
            "no-remote": "1",
            "no-sandbox": "1",
            # `rcodesign` gets a secure timestamp from Apple's server, which notarization requires.
            "requires-network": "1",
        }

    args.add(ctx.file.src)
    args.add(ctx.outputs.out)

    ctx.actions.run(
        executable = ctx.toolchains[_CODESIGN_TOOLCHAIN].codesign,
        arguments = [args],
        inputs = [ctx.file.src],
        outputs = [ctx.outputs.out],
        execution_requirements = execution_requirements,
        mnemonic = "MacosSign",
        progress_message = "Signing %{output} " + signer,
    )

    return [DefaultInfo(files = depset([ctx.outputs.out]))]

macos_sign = rule(
    implementation = _macos_sign_impl,
    doc = "Signs a thin or universal Mach-O file, ad hoc or with the identity that the flags give.",
    attrs = {
        "identifier": attr.string(
            doc = "The code signing identifier, like `mirrord`, which the allow lists of " +
                  "endpoint security tools match.",
            mandatory = True,
        ),
        "out": attr.output(
            doc = "The signed file.",
            mandatory = True,
        ),
        "src": attr.label(
            doc = "The Mach-O file to sign.",
            allow_single_file = True,
            mandatory = True,
        ),
        "_p12_file": attr.label(default = "//mirrord/cli:macos_signing_p12_file"),
        "_p12_password_file": attr.label(
            default = "//mirrord/cli:macos_signing_p12_password_file",
        ),
    },
    toolchains = [_CODESIGN_TOOLCHAIN],
)
