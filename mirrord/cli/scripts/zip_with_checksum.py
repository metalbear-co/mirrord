"""Zips the CLI as `mirrord`, and writes the zip's SHA-256 checksum in the format of
`shasum -a 256`, which the Homebrew formula takes.
"""

import argparse
import hashlib
import shutil
import zipfile
from pathlib import Path

DESCRIPTION = """\
Zips a build of the CLI as `mirrord`, and writes the zip's SHA-256 checksum, as the release
publishes them.
"""

EPILOG = """\
how to use it:
  The release files build it for each platform's CLI:

    bazel build //mirrord/cli:release_unix

  Zip a CLI by hand:

    bazel run //mirrord/cli:zip_with_checksum -- <cli> <zip> <checksum>

  The zip is the same for the same CLI, whenever and wherever it's built.
"""


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //mirrord/cli:zip_with_checksum --",
        description=DESCRIPTION,
        epilog=EPILOG,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("cli", type=Path, help="the CLI to zip")
    parser.add_argument("zip", type=Path, help="the zip to write")
    parser.add_argument("checksum", type=Path, help="the file to write the zip's checksum into")
    arguments = parser.parse_args()

    # A fixed modification time makes the zip reproducible.
    entry = zipfile.ZipInfo("mirrord", date_time=(1980, 1, 1, 0, 0, 0))
    entry.compress_type = zipfile.ZIP_DEFLATED
    # `unzip` reads the file type and mode (here a regular file with mode 755) from the upper 16
    # bits.
    entry.external_attr = 0o100755 << 16
    # 3 is Unix. `unzip` only reads the type and mode of an entry made on Unix.
    entry.create_system = 3
    # `ZipFile.open` writes the header before the data, so it decides from this size whether the
    # entry needs ZIP64.
    entry.file_size = arguments.cli.stat().st_size

    with (
        arguments.cli.open("rb") as cli,
        zipfile.ZipFile(arguments.zip, "w") as archive,
        archive.open(entry, "w") as zipped_cli,
    ):
        shutil.copyfileobj(cli, zipped_cli)

    with arguments.zip.open("rb") as archive:
        digest = hashlib.file_digest(archive, "sha256").hexdigest()

    arguments.checksum.write_text(f"{digest}  {arguments.zip.name}\n")


if __name__ == "__main__":
    main()
