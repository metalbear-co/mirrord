"""Writes the ID of the layers that the standalone CLI embeds, as the line
`MIRRORD_BUNDLED_LAYER_ID=<id>` of a `rustc_env_files` file.

The CLI names the files that it writes the layers to after the ID, so that it never loads a layer
that another build wrote. The ID is the start of a SHA-256 digest of the layers, so a build of the
same layers gets the same ID, and the CLI the same bytes.

Usage: layer_id.py <output> <layer>...
"""

import hashlib
import sys
from pathlib import Path


def main():
    output, *layers = sys.argv[1:]

    # Hashes the digest of each layer, rather than their bytes one after another, so that the
    # layers' boundaries count too.
    digest = hashlib.sha256()
    for layer in layers:
        digest.update(hashlib.sha256(Path(layer).read_bytes()).digest())

    Path(output).write_text(f"MIRRORD_BUNDLED_LAYER_ID={digest.hexdigest()[:16]}\n")


if __name__ == "__main__":
    main()
