"""Renders mirrord's logos into the block-character art that the terminal interface shows on its
home screen, as `bazel run //scripts:render_tui_logo`.

Each character carries two vertically stacked pixels through the half-block glyphs, so the art has
twice the vertical resolution of the character grid it occupies. Only the glyph is stored, never a
colour: the interface draws the art in the brand colour and leaves the terminal's own background
showing through, the way the rest of its palette works.

The logo is line art, dark strokes around a pale mirror, so it's the ink that becomes a lit
character. The source is flattened onto white first, which puts the transparent surround at the
same value as the mirror's interior and leaves only the strokes below the threshold.
"""

import argparse
import io
import os
from pathlib import Path

import resvg_py
from PIL import Image

# The art in `mirrord/tui/resources/`, each with its source, its width in columns, and the
# threshold below which a pixel is ink.
#
# A higher threshold counts more of the anti-aliased edge as ink, which thickens the strokes; below
# roughly 120 the eyes drop out at these sizes. Re-tune it by eye after any change to the size or
# the source, since the right value depends on both.
LOGOS = {
    "logo-big": ("images/logo.svg", 100, 150),
    "logo-small": ("images/icon.png", 27, 205),
}

# Two pixel rows collapse into one character row: which of the pair is ink picks between the two
# half blocks, the full block and a space.
GLYPHS = {(False, False): " ", (True, False): "▀", (False, True): "▄", (True, True): "█"}

DESCRIPTION = "Renders mirrord's logos into the art that the terminal interface shows.\n"

EPILOG = """\
how to use it:
  Render all of the art again, after a change to a source image:

    bazel run //scripts:render_tui_logo

  Try another width or threshold for one piece while you tune it by eye, then put the values that
  you keep into LOGOS, in this script:

    bazel run //scripts:render_tui_logo -- logo-big --threshold 160
"""


def render(source, columns, threshold):
    """Returns the rows of block characters that show the image at `source`."""
    if source.suffix == ".svg":
        image = Image.open(io.BytesIO(bytes(resvg_py.svg_to_bytes(svg_path=str(source)))))
    else:
        image = Image.open(source)

    image = image.convert("RGBA")
    gray = Image.alpha_composite(Image.new("RGBA", image.size, "white"), image).convert("L")

    # Sub-pixels are half a character tall, so on a typical terminal cell they come out roughly
    # square, and sampling twice as many rows as columns preserves the source's proportions.
    gray = gray.resize((columns, round(image.height / image.width * columns)), Image.Resampling.LANCZOS)
    width, height = gray.size
    pixels = gray.load()
    assert pixels is not None

    def ink(x, y):
        """Whether the pixel at (x, y) is dark enough to draw. Past the edge is never ink."""
        if y >= height or x >= width:
            return False
        value = pixels[x, y]
        assert isinstance(value, int)
        return value < threshold

    rows = ["".join(GLYPHS[(ink(x, top), ink(x, top + 1))] for x in range(columns)) for top in range(0, height, 2)]

    # The source images carry their own margin. Left in, it would be blank rows that the home
    # screen still reserves height for, and still measures when it decides whether the art fits.
    while rows and not rows[0].strip():
        rows.pop(0)
    while rows and not rows[-1].strip():
        rows.pop()

    # Padded rather than trimmed: the art is drawn left-aligned in an area of exactly this width,
    # so every row has to span it, or the image skews.
    return [row.ljust(columns) for row in rows]


def main():
    parser = argparse.ArgumentParser(
        prog="bazel run //scripts:render_tui_logo --",
        description=DESCRIPTION,
        epilog=EPILOG
        + "\nthe art:\n"
        + "\n".join(
            f"  {name:<10}  {source}, {columns} columns, threshold {threshold}"
            for name, (source, columns, threshold) in LOGOS.items()
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "names",
        nargs="*",
        metavar="name",
        help="render only these, from the list below, rather than all of them",
    )
    parser.add_argument("--columns", type=int, help="the width in columns, instead of the one in the list")
    parser.add_argument(
        "--threshold",
        type=int,
        choices=range(1, 255),
        metavar="1-254",
        help="the threshold below which a pixel is ink, instead of the one in the list",
    )
    arguments = parser.parse_args()

    unknown = [name for name in arguments.names if name not in LOGOS]
    if unknown:
        parser.error(f"unknown art {unknown}, choose from {list(LOGOS)}")

    workspace = Path(os.environ["BUILD_WORKSPACE_DIRECTORY"])
    for name in arguments.names or LOGOS:
        source, columns, threshold = LOGOS[name]
        rows = render(
            workspace / source,
            arguments.columns or columns,
            arguments.threshold or threshold,
        )
        output = workspace / "mirrord" / "tui" / "resources" / name
        output.write_text("".join(f"{row}\n" for row in rows), encoding="utf-8")
        print(f"wrote {output}")


if __name__ == "__main__":
    main()
