"""A rule that code-signs a Mach-O file, such as the universal layer or the universal CLI.

The rule signs with `rcodesign`, from the apple-codesign project, which the `codesign.bzl` module
provides as a prebuilt toolchain for each OS. Unlike Apple's `codesign`, `rcodesign` runs on Linux
too, and it reads the signing key from a file rather than from a macOS keychain, so the Mac
binaries can be built and signed on any machine.

By default, the rule signs ad hoc: the signature holds the hashes of the code, without a
certificate. An ad hoc signature depends only on the file, so the action is hermetic, and Bazel
caches it and can run it remotely, like any other action.

With `--//bazel:macos_signing_p12_file` and `--//bazel:macos_signing_p12_password_file`, the rule
signs with the certificate and private key in that PKCS #12 file instead, as a release does with
MetalBear's Developer ID certificate. The flags hold the files' paths, not their contents, so the
secrets never become inputs of the action: they don't enter the action key or the cache, and
remote execution can't upload them. Because Bazel can't see the files, it can't tell when they
change either, so these actions run locally, outside the sandbox (Linux's sandbox hides `/tmp`, for
example), and skip the remote cache and the disk cache. They also need the network, because the
signature gets a secure timestamp from Apple's timestamp server, which notarization requires.

`rcodesign` signs every slice of a universal file, and replaces the signatures that the slices
already have, like the ad hoc ones that the linker writes. So the slices don't need signing before
`llvm-lipo` combines them: signing the slices first and then the universal file gives the same
bytes as signing only the universal file.
"""

load("@bazel_skylib//rules:common_settings.bzl", "BuildSettingInfo")

_CODESIGN_TOOLCHAIN = "@codesign.bzl//toolchain:toolchain_type"

def _macos_sign_impl(ctx):
    p12_file = ctx.attr._p12_file[BuildSettingInfo].value
    p12_password_file = ctx.attr._p12_password_file[BuildSettingInfo].value
    if bool(p12_file) != bool(p12_password_file):
        fail("--//bazel:macos_signing_p12_file and --//bazel:macos_signing_p12_password_file " +
             "must be set together")

    args = ctx.actions.args()
    args.add("sign")
    args.add("--binary-identifier", ctx.attr.identifier)

    # The hardened runtime takes effect only in the main program of a process, so it matters for
    # the CLI and not for the layer. The layer is signed with it too, so that both files are signed
    # the same way.
    args.add("--code-signature-flags", "runtime")

    execution_requirements = {}
    if p12_file:
        args.add("--p12-file", p12_file)
        args.add("--p12-password-file", p12_password_file)
        execution_requirements = {
            "no-cache": "1",
            "no-remote": "1",
            "no-sandbox": "1",
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
        progress_message = "Signing %{output}" + (" with " + p12_file if p12_file else " ad hoc"),
    )

    return [DefaultInfo(files = depset([ctx.outputs.out]))]

macos_sign = rule(
    implementation = _macos_sign_impl,
    doc = "Signs a thin or universal Mach-O file, ad hoc or with the identity that the flags give.",
    attrs = {
        "identifier": attr.string(
            doc = "The code signing identifier, which allow lists of endpoint security tools can " +
                  "match. Apple's `codesign` derives it from the file's name, and `rcodesign` " +
                  "needs it given.",
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
        "_p12_file": attr.label(default = "//bazel:macos_signing_p12_file"),
        "_p12_password_file": attr.label(default = "//bazel:macos_signing_p12_password_file"),
    },
    toolchains = [_CODESIGN_TOOLCHAIN],
)
