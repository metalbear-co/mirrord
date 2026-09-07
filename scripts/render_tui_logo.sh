#!/bin/sh
# Renders one of the mirrord logo files into the block-character art the terminal interface
# shows on its home screen, and writes it into mirrord/tui/resources/.
# It should be run from the repo root directory.
#
# Each character carries two vertically stacked pixels through the half-block glyphs, so the
# art has twice the vertical resolution of the character grid it occupies. Only the glyph is
# stored, never a colour: the interface draws the art in the brand colour and leaves the
# terminal's own background showing through, the way the rest of its palette works.
#
# The logo is line art - dark strokes around a pale mirror - so it is the *ink* that becomes
# a lit character. The source is flattened onto white first, which puts the transparent
# surround at the same value as the mirror's interior and leaves only the strokes below the
# threshold.
#
# Arguments: <image> <width in columns> <threshold 1-254> <output name>
#
# The checked-in art was produced with:
#   ./scripts/render_tui_logo.sh images/logo.svg 100 150 logo-big
#   ./scripts/render_tui_logo.sh images/icon.png  27 205 logo-small
#
# A higher threshold counts more of the anti-aliased edge as ink, which thickens the strokes;
# below roughly 120 the eyes drop out at these sizes. Re-tune it by eye after any change to
# the size or the source, since the right value depends on both.
#
# Requires ImageMagick (`brew install imagemagick`). Only needed to regenerate the art, so it
# is not part of any build.

set -e

if [ $# -ne 4 ]; then
  echo "usage: $0 <image> <width in columns> <threshold 1-254> <output name>" >&2
  exit 1
fi

image=$1
columns=$2
threshold=$3
name=$4
out="mirrord/tui/resources/$name"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Sub-pixels are half a character tall, so on a typical terminal cell they come out roughly
# square and sampling twice as many rows as columns preserves the source's proportions.
rows=$(magick identify -format "%[fx:round(h/w*$columns)]" "$image")
magick -background white "$image" -alpha remove -colorspace gray \
  -resize "${columns}x${rows}!" -depth 8 "$tmp/gray.pgm"

python3 - "$tmp/gray.pgm" "$columns" "$threshold" > "$out" <<'PY'
import sys

path, columns, threshold = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
data = open(path, "rb").read()

# Binary PGM: the magic, width, height and maxval as whitespace-separated ASCII, then one
# byte per pixel. The format allows comment lines, but ImageMagick does not emit them.
magic, width, height, _maxval, pixels = data.split(maxsplit=4)
assert magic == b"P5", magic
width, height = int(width), int(height)


def ink(x, y):
    """Whether the sub-pixel at (x, y) is dark enough to draw. Past the edge is never ink."""
    if y >= height or x >= width:
        return False
    return pixels[y * width + x] < threshold


# Two pixel rows collapse into one character row: which of the pair is ink picks between the
# two half blocks, the full block and a space.
GLYPHS = {(False, False): " ", (True, False): "▀", (False, True): "▄", (True, True): "█"}

rows = [
    "".join(GLYPHS[(ink(x, top), ink(x, top + 1))] for x in range(columns))
    for top in range(0, height, 2)
]

# The source images carry their own margin. Left in, it would be blank rows that the home
# screen still reserves height for and still measures when deciding whether the art fits.
while rows and not rows[0].strip():
    rows.pop(0)
while rows and not rows[-1].strip():
    rows.pop()

for row in rows:
    # Padded rather than trimmed: the art is drawn left-aligned in an area of exactly this
    # width, so every row has to span it or the image skews.
    print(row.ljust(columns))
PY

echo "wrote $out"
