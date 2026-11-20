"""Writes the configuration reference that GitBook publishes, from the doc comments of mirrord-config,
as `bazel run //medschool:residency`.

medschool turns the doc comments into one Markdown file, and residency splits it into one page per
top-level heading, each with the front matter that GitBook reads.
"""

import argparse
import datetime
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from mirrord_tools import runfile_program

# The pages, in the order of their top-level headings in medschool's output, each with its front
# matter. `{lastmod}` becomes the date of the run.
PAGES = {
    "README.md": """\
---
title: Configuration Examples
date: 2023-05-17T12:59:39.000Z
lastmod: {lastmod}
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 110
toc: true
tags:
  - open source
  - team
  - enterprise
description: Configure mirrord with a mirrord.json file, from a minimal setup to templated values and complete example configurations.
---
""",
    "options.md": """\
---
title: Configuration Options
date: 2023-05-17T12:59:39.000Z
lastmod: {lastmod}
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 110
toc: true
tags:
  - open source
  - team
  - enterprise
description: >-
  Detailed documentation of all configuration options available for mirrord,
  including usage, defaults, and examples.
---
""",
}

DESCRIPTION = "Writes the pages of mirrord's configuration reference from mirrord-config's doc comments.\n"

EPILOG = """\
how to use it:
  Write the pages into a directory, which it creates if it doesn't exist:

    bazel run //medschool:residency -- <directory>

  Each top-level heading of the generated reference starts a page, so a new top-level heading needs
  a page of its own in PAGES, in this script.
"""


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //medschool:residency --",
        description=DESCRIPTION,
        epilog=EPILOG + "\nthe pages:\n" + "\n".join(f"  {page}" for page in PAGES),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "directory",
        type=Path,
        help="where the pages go, relative to the directory that you run `bazel run` in",
    )
    arguments = parser.parse_args()

    medschool = runfile_program("medschool/medschool")

    workspace = Path(os.environ["BUILD_WORKSPACE_DIRECTORY"])
    directory = Path(arguments.directory).absolute()
    directory.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as scratch:
        reference = Path(scratch, "reference.md")
        subprocess.run(
            [
                medschool,
                "--root-type",
                "LayerConfig",
                "--input",
                workspace / "mirrord" / "config" / "src",
                "--output",
                reference,
            ],
            check=True,
        )
        text = reference.read_text(encoding="utf-8")

    sections = [section for section in re.split(r"(?m)^(?=# )", text) if section]
    if len(sections) != len(PAGES):
        headings = re.findall(r"(?m)^# .*", text)
        sys.exit(
            f"The reference has {len(headings)} top-level headings, {headings}, but residency has "
            f"{len(PAGES)} pages, {list(PAGES)}. Add a page for each new heading to PAGES."
        )

    lastmod = datetime.date.today().strftime("%Y-%m-%dT00:00:00.000Z")
    for (page, front_matter), section in zip(PAGES.items(), sections):
        # An empty line separates the front matter from the page.
        content = front_matter.replace("{lastmod}", lastmod) + "\n" + section.rstrip("\n") + "\n"
        (directory / page).write_text(content, encoding="utf-8")
        print(f"wrote {directory / page}")


if __name__ == "__main__":
    main()
