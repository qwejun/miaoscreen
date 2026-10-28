"""Crops a panel screenshot to the circle the glass actually shows.

The panel is an 800x800 surface behind a round bezel: the corners of every
screenshot are pixels nobody sees, and they are exactly the pixels that make a
centred line look off centre. This masks them out so a preview matches the
device.

    python scripts/round-preview.py in.png out.png [scale]
"""

import sys

from PIL import Image, ImageDraw

SOURCE = sys.argv[1]
TARGET = sys.argv[2]
SCALE = float(sys.argv[3]) if len(sys.argv) > 3 else 1.0

image = Image.open(SOURCE).convert("RGB")
width, height = image.size
diameter = min(width, height)

# Supersample the mask edge: a hard circle at this size shows stair-steps that
# the real bezel does not have. Coordinates are in the supersampled space, so
# every surface coordinate is multiplied by OVERSAMPLE.
OVERSAMPLE = 4
mask = Image.new("L", (width * OVERSAMPLE, height * OVERSAMPLE), 0)
ImageDraw.Draw(mask).ellipse(
    (
        (width - diameter) * OVERSAMPLE // 2,
        (height - diameter) * OVERSAMPLE // 2,
        (width + diameter) * OVERSAMPLE // 2,
        (height + diameter) * OVERSAMPLE // 2,
    ),
    fill=255,
)
mask = mask.resize((width, height), Image.LANCZOS)

black = Image.new("RGB", (width, height), (0, 0, 0))
masked = Image.composite(image, black, mask)

if SCALE != 1.0:
    masked = masked.resize(
        (int(width * SCALE), int(height * SCALE)), Image.LANCZOS
    )
masked.save(TARGET)
print(f"{TARGET} {masked.size[0]}x{masked.size[1]}")
