"""Zips the CLI as `mirrord`, and writes the zip's SHA-256 checksum in the format of `shasum -a 256`.

Usage: zip_with_checksum.py <cli> <out.zip> <out.shasum256>

The zip holds one executable entry with a fixed time, so the same CLI always zips to the same bytes.
The CLI and its zip are hundreds of megabytes each, so the script copies and hashes them in chunks
rather than reading them whole.
"""

import hashlib
import os
import shutil
import sys
import zipfile

cli_path, zip_path, checksum_path = sys.argv[1:]

entry = zipfile.ZipInfo("mirrord", date_time=(1980, 1, 1, 0, 0, 0))
entry.compress_type = zipfile.ZIP_DEFLATED
# A regular file with mode 755, in the upper half, where Unix's `unzip` reads it from.
entry.external_attr = 0o100755 << 16
entry.create_system = 3
# `ZipFile.open` decides from the size whether the entry needs the ZIP64 format.
entry.file_size = os.path.getsize(cli_path)

with (
    open(cli_path, "rb") as cli,
    zipfile.ZipFile(zip_path, "w") as archive,
    archive.open(entry, "w") as zipped_cli,
):
    shutil.copyfileobj(cli, zipped_cli)

with open(zip_path, "rb") as archive:
    digest = hashlib.file_digest(archive, "sha256").hexdigest()

with open(checksum_path, "w") as checksum:
    checksum.write(f"{digest}  {os.path.basename(zip_path)}\n")
