"""Draw one BongoCat skin from nothing but geometry and a palette.

A keyboard skin is three layers over the same 612x354 canvas, and every layer
has to agree with the other two to the pixel: the background paints the key
caps, each key image paints one paw landing on one cap, and the paw only looks
like it lands on anything if both were laid out by the same function. So this
script owns one layout table and one projection, and derives all of it from
them. Nothing here is traced from another skin.

The character is drawn in crayon: flat fills, a heavy dark outline, and a paper
grain over the whole canvas. That is the whole style — no gradients, no
anti-aliased softness except the supersample the renderer gets for free.

Run from the repository root:

    python scripts/make-shin-chan-skin.py

It rewrites desktop/resources/models/crayon-shin-chan-keyboard/ in place.
"""

from __future__ import annotations

import json
import math
import pathlib
import random
import shutil

from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
MODEL = ROOT / "desktop" / "resources" / "models" / "crayon-shin-chan-keyboard"
SOURCE = ROOT / "desktop" / "resources" / "models" / "nalin-keyboard"

CANVAS = (612, 354)
SS = 4  # supersample factor; every shape is drawn at 4x then shrunk once

# The palette. Red and yellow are the character's clothes; the ink is warm black
# rather than pure black because crayon never lays down a true black.
INK = (51, 48, 44)
PAPER = (253, 248, 239)
SKIN = (243, 198, 160)
SKIN_DARK = (214, 160, 120)
BLUSH = (244, 154, 147)
RED = (232, 69, 60)
RED_DEEP = (198, 50, 44)
YELLOW = (247, 201, 72)
YELLOW_LIGHT = (252, 232, 170)
WHITE = (255, 253, 248)
DECK = (247, 216, 120)

# Projection. The deck is laid out as a plain rectangle and then tilted as a
# whole, which keeps the grid exact: a key's centre and the paw that lands on it
# are computed by the same function, so they cannot drift apart as the deck
# turns. `deck_point` is the only place that knows about the tilt.
DECK_TILT = 4.0            # degrees, positive lifts the right end of the deck
DECK_PIVOT = (304.0, 243.0)
GRID_ORIGIN = (66.0, 168.0)
PITCH_X, PITCH_Y = 34.0, 30.0
CAP_W, CAP_H = 28.5, 23.5
CAP_CORNER = 5.0

# Caps that are not letters or digits get the character's colours, so the deck
# reads as a keyboard rather than a field of confetti.
FUNCTION_CAPS = {
    "Backspace", "BackSlash", "Tab", "CapsLock", "Return", "Shift", "ControlLeft",
    "Meta", "Alt", "Space",
    "UpArrow", "DownArrow", "LeftArrow", "RightArrow",
}

# (name, side, column, row, span in columns)
#
# The split follows the artwork rule the renderer reads: whatever sits in
# left-keys is pressed by the left paw. The left hand owns the six columns it
# actually covers on a real keyboard, and the right hand owns the rest, which is
# why the number row is cut at 5 and the right hand starts at 6.
LAYOUT: list[tuple[str, str, int, int, int]] = [
    ("BackQuote", "left", 0, 0, 1),
    ("Num1", "left", 1, 0, 1),
    ("Num2", "left", 2, 0, 1),
    ("Num3", "left", 3, 0, 1),
    ("Num4", "left", 4, 0, 1),
    ("Num5", "left", 5, 0, 1),
    ("Num6", "right", 6, 0, 1),
    ("Num7", "right", 7, 0, 1),
    ("Num8", "right", 8, 0, 1),
    ("Num9", "right", 9, 0, 1),
    ("Num0", "right", 10, 0, 1),
    ("Minus", "right", 11, 0, 1),
    ("Equal", "right", 12, 0, 1),
    ("Backspace", "right", 13, 0, 1),
    ("Tab", "left", 0, 1, 1),
    ("KeyQ", "left", 1, 1, 1),
    ("KeyW", "left", 2, 1, 1),
    ("KeyE", "left", 3, 1, 1),
    ("KeyR", "left", 4, 1, 1),
    ("KeyT", "left", 5, 1, 1),
    ("KeyY", "right", 6, 1, 1),
    ("KeyU", "right", 7, 1, 1),
    ("KeyI", "right", 8, 1, 1),
    ("KeyO", "right", 9, 1, 1),
    ("KeyP", "right", 10, 1, 1),
    ("LeftBracket", "right", 11, 1, 1),
    ("RightBracket", "right", 12, 1, 1),
    ("BackSlash", "right", 13, 1, 1),
    ("CapsLock", "left", 0, 2, 1),
    ("KeyA", "left", 1, 2, 1),
    ("KeyS", "left", 2, 2, 1),
    ("KeyD", "left", 3, 2, 1),
    ("KeyF", "left", 4, 2, 1),
    ("KeyG", "left", 5, 2, 1),
    ("KeyH", "right", 6, 2, 1),
    ("KeyJ", "right", 7, 2, 1),
    ("KeyK", "right", 8, 2, 1),
    ("KeyL", "right", 9, 2, 1),
    ("SemiColon", "right", 10, 2, 1),
    ("Quote", "right", 11, 2, 1),
    ("Return", "right", 12, 2, 2),
    ("Shift", "left", 0, 3, 1),
    ("KeyZ", "left", 1, 3, 1),
    ("KeyX", "left", 2, 3, 1),
    ("KeyC", "left", 3, 3, 1),
    ("KeyV", "left", 4, 3, 1),
    ("KeyB", "left", 5, 3, 1),
    ("KeyN", "right", 6, 3, 1),
    ("KeyM", "right", 7, 3, 1),
    ("Comma", "right", 8, 3, 1),
    ("Dot", "right", 9, 3, 1),
    ("Slash", "right", 10, 3, 1),
    ("Shift", "right", 11, 3, 1),
    ("ControlLeft", "left", 0, 4, 1),
    ("Meta", "left", 1, 4, 1),
    ("Alt", "left", 2, 4, 1),
    ("Space", "left", 3, 4, 5),
    ("UpArrow", "right", 8, 4, 1),
    ("LeftArrow", "right", 9, 4, 1),
    ("DownArrow", "right", 10, 4, 1),
    ("RightArrow", "right", 11, 4, 1),
]


def _tilt(x: float, y: float) -> tuple[float, float]:
    """Turn a point of the flat grid into a point of the tilted deck.

    Screen y grows downward, so a positive angle lifts the right end. This is the
    only tilt in the file: the deck, the caps and every paw all come through
    here, which is why the artwork stays registered to the caps.
    """
    theta = math.radians(DECK_TILT)
    dx, dy = x - DECK_PIVOT[0], y - DECK_PIVOT[1]
    return (
        DECK_PIVOT[0] + dx * math.cos(theta) + dy * math.sin(theta),
        DECK_PIVOT[1] - dx * math.sin(theta) + dy * math.cos(theta),
    )


def deck_point(col: float, row: float, span: float = 1.0) -> tuple[float, float]:
    """Where the centre of a key sits on the canvas."""
    x = GRID_ORIGIN[0] + PITCH_X * (col + span / 2.0)
    y = GRID_ORIGIN[1] + PITCH_Y * (row + 0.5)
    return _tilt(x, y)


def cap_centre(col: int, row: int, span: int) -> tuple[float, float]:
    return deck_point(col, row, span)


def cap_width(span: int) -> float:
    return CAP_W + (span - 1) * PITCH_X


def keyed_layout() -> dict[str, tuple[str, int, int, int]]:
    return {name: (side, col, row, span) for name, side, col, row, span in LAYOUT}


def cap_colour(name: str, col: int, row: int) -> tuple[int, int, int]:
    """Letters are paper; the number row and the keys around it carry the outfit."""
    if name not in FUNCTION_CAPS:
        return WHITE
    # Space is the one long key: white keeps it from shouting, and the red is
    # saved for the small caps that ring the deck.
    if name == "Space":
        return WHITE
    return RED


def grain(size: tuple[int, int], strength: int = 7) -> Image.Image:
    """A paper tooth, painted over everything at the end.

    Crayon on paper leaves the low spots bare. A sparse noise layer at low alpha
    reads as that without touching the palette. `effect_noise` is a single C call
    and produces the same even speckle a per-pixel loop would, in no time at 4x.
    """
    noise = Image.effect_noise(size, strength * 5).convert("L")
    # Squeeze the range so the veil stays a whisper rather than a dirty wash.
    return noise.point(lambda value: value // 3)


def rounded_cap(
    cx: float,
    cy: float,
    width: float,
    height: float,
    corner: float,
) -> list[tuple[float, float]]:
    """A rounded rectangle expressed as an octagon, already tilted.

    PIL cannot rotate a rounded rectangle into place, and pasting a rotated tile
    per cap costs its own edge artefacts. An octagon with its four corners cut
    is indistinguishable from a rounded rect at this size, and it tilts for free.
    """
    hw, hh = width / 2.0, height / 2.0
    r = min(corner, hw, hh)
    local = [
        (-hw + r, -hh), (hw - r, -hh), (hw, -hh + r), (hw, hh - r),
        (hw - r, hh), (-hw + r, hh), (-hw, hh - r), (-hw, -hh + r),
    ]
    # A crayon does not draw a straight line. Two thirds of a pixel of wobble
    # per corner is invisible one cap at a time and unmistakable across sixty.
    rng = random.Random(int(cx * 97) ^ int(cy * 89))
    return [
        _tilt(cx + px + rng.uniform(-0.65, 0.65), cy + py + rng.uniform(-0.65, 0.65))
        for px, py in local
    ]


def build_background() -> Image.Image:
    """The deck plate and its caps, tilted as one board."""
    width, height = CANVAS[0] * SS, CANVAS[1] * SS
    image = Image.new("RGBA", (width, height), PAPER + (255,))
    draw = ImageDraw.Draw(image)

    def s(v: float) -> float:
        return v * SS

    # Deck plate: the grid's own bounds, pushed out far enough to sit under the
    # outermost caps, tilted as a single board.
    margin_x, margin_y = 18.0, 16.0
    left = GRID_ORIGIN[0] - margin_x
    right = GRID_ORIGIN[0] + PITCH_X * 14 + margin_x
    top = GRID_ORIGIN[1] - margin_y
    bottom = GRID_ORIGIN[1] + PITCH_Y * 5 + margin_y
    plate = [
        _tilt(left, top), _tilt(right, top), _tilt(right, bottom), _tilt(left, bottom),
    ]
    draw.polygon([(s(x), s(y)) for x, y in plate], fill=DECK, outline=INK)
    draw.line([(s(x), s(y)) for x, y in plate + plate[:1]], fill=INK, width=max(1, int(s(2.0))))

    # A second, inset line reads as the deck's raised lip without any shading.
    inset = 6.0
    lip = [
        _tilt(left + inset, top + inset),
        _tilt(right - inset, top + inset),
        _tilt(right - inset, bottom - inset),
        _tilt(left + inset, bottom - inset),
    ]
    draw.line([(s(x), s(y)) for x, y in lip + lip[:1]], fill=(206, 190, 166), width=max(1, int(s(1.0))))

    for name, side, col, row, span in LAYOUT:
        cx, cy = cap_centre(col, row, span)
        colour = cap_colour(name, col, row)
        # The cap's own side wall, drawn first and a couple of pixels lower.
        # Flat colour needs this: without a thickness the caps look printed on
        # the plate instead of standing on it.
        wall = rounded_cap(cx, cy + 2.4, cap_width(span), CAP_H, CAP_CORNER)
        draw.polygon(
            [(s(x), s(y)) for x, y in wall],
            fill=tuple(int(v * 0.58) for v in colour),
        )
        points = rounded_cap(cx, cy, cap_width(span), CAP_H, CAP_CORNER)
        draw.polygon(
            [(s(x), s(y)) for x, y in points],
            fill=colour,
            outline=INK,
        )
        draw.line(
            [(s(x), s(y)) for x, y in points + points[:1]],
            fill=INK,
            width=max(1, int(s(1.15))),
        )

    image = image.resize(CANVAS, Image.LANCZOS)
    return apply_grain(image)


def draw_paw(size: tuple[int, int], mirrored: bool) -> Image.Image:
    """One hand, palm up the canvas and four fingers reaching down onto a key.

    Drawn on its own transparent tile so a key image is a paste, not a redraw.
    The proportions are the character's: a wide flat palm, fingers as thick as
    they are long, and a thumb that sticks out sideways rather than forward.
    """
    width, height = size[0] * SS, size[1] * SS
    image = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    def s(v: float) -> float:
        return v * SS

    cx = width / 2
    palm_w = width * 0.76
    palm_h = height * 0.56
    palm_top = height * 0.02
    palm_bottom = palm_top + palm_h

    # Four fingers nearly touching: at this size a gap reads as a comb, not a
    # hand. The outline alone separates them.
    finger_w = palm_w * 0.235
    finger_h = height * 0.26
    stroke = max(1, int(s(1.5)))

    # Fingers first, so the palm covers their roots.
    for index, offset in enumerate((-0.325, -0.108, 0.108, 0.325)):
        fx = cx + offset * palm_w
        reach = finger_h * (1.0 if index in (1, 2) else 0.80)
        draw.rounded_rectangle(
            (fx - finger_w / 2, palm_bottom - finger_h * 0.55,
             fx + finger_w / 2, palm_bottom + reach),
            radius=finger_w / 2,
            fill=SKIN,
            outline=INK,
            width=stroke,
        )

    # Thumb: a short capsule off the side, tip pointing away from the palm.
    side = 1.0 if mirrored else -1.0
    thumb_w, thumb_h = palm_w * 0.34, palm_h * 0.94
    thumb_cx = cx + side * (palm_w * 0.54)
    thumb_cy = palm_bottom - palm_h * 0.42
    draw.rounded_rectangle(
        (thumb_cx - thumb_w / 2, thumb_cy - thumb_h / 2,
         thumb_cx + thumb_w / 2, thumb_cy + thumb_h / 2),
        radius=thumb_w / 2,
        fill=SKIN,
        outline=INK,
        width=stroke,
    )

    # Palm over the finger roots.
    draw.rounded_rectangle(
        (cx - palm_w / 2, palm_top, cx + palm_w / 2, palm_bottom),
        radius=palm_w * 0.30,
        fill=SKIN,
        outline=INK,
        width=max(1, int(s(1.8))),
    )
    # Two creases at the wrist end: enough to read as a hand at 40 px, no more.
    for offset in (-0.20, 0.20):
        draw.line(
            [(cx + offset * palm_w, palm_top + palm_h * 0.30),
             (cx + offset * palm_w * 1.06, palm_top + palm_h * 0.62)],
            fill=SKIN_DARK,
            width=max(1, int(s(1.3))),
        )

    image = image.resize(size, Image.LANCZOS)
    if mirrored:
        image = image.transpose(Image.FLIP_LEFT_RIGHT)
    return image


def build_key_image(side: str, col: int, row: int, span: int) -> Image.Image:
    """A transparent canvas with one paw sitting on one cap."""
    canvas = Image.new("RGBA", CANVAS, (0, 0, 0, 0))
    cx, cy = cap_centre(col, row, span)

    # The paw is bigger than the cap and hangs a little above it, the way a
    # hand covers a key from the back of the keyboard.
    paw_w = min(66, 38 + span * 6)
    paw_h = 44
    paw = draw_paw((paw_w, paw_h), mirrored=(side == "right"))
    canvas.alpha_composite(
        paw,
        (int(round(cx - paw_w / 2)), int(round(cy - paw_h * 0.62))),
    )
    return canvas


def apply_grain(image: Image.Image, strength: int = 6) -> Image.Image:
    """Lay a paper tooth over a finished layer without shifting its colours."""
    alpha = grain(image.size, strength)
    veil = Image.new("RGB", image.size, (120, 96, 70))
    layer = Image.merge("RGBA", veil.split() + (alpha,))
    return Image.alpha_composite(image.convert("RGBA"), layer)


def build_cover() -> Image.Image:
    """The library card: the character's face, drawn to fill 624x362."""
    width, height = 624, 362
    image = Image.new("RGBA", (width * SS, height * SS), PAPER + (255,))
    draw = ImageDraw.Draw(image)

    def s(v: float) -> float:
        return v * SS

    cx, cy = width * 0.5, height * 0.56

    # Head: a wide egg, the way a five-year-old's face is drawn.
    head_w, head_h = 210, 196
    draw.ellipse(
        (s(cx - head_w / 2), s(cy - head_h / 2), s(cx + head_w / 2), s(cy + head_h / 2)),
        fill=SKIN, outline=INK, width=int(s(2.2)),
    )
    # Hair: a cap over the top third with two notches cut into the fringe.
    hair = [
        (s(cx - head_w / 2 - 4), s(cy - head_h * 0.16)),
        (s(cx - head_w / 2 - 6), s(cy - head_h * 0.46)),
        (s(cx - head_w * 0.28), s(cy - head_h * 0.56)),
        (s(cx - head_w * 0.10), s(cy - head_h * 0.50)),
        (s(cx), s(cy - head_h * 0.57)),
        (s(cx + head_w * 0.12), s(cy - head_h * 0.50)),
        (s(cx + head_w * 0.30), s(cy - head_h * 0.56)),
        (s(cx + head_w / 2 + 6), s(cy - head_h * 0.44)),
        (s(cx + head_w / 2 + 4), s(cy - head_h * 0.14)),
        (s(cx + head_w * 0.30), s(cy - head_h * 0.22)),
        (s(cx + head_w * 0.12), s(cy - head_h * 0.16)),
        (s(cx), s(cy - head_h * 0.23)),
        (s(cx - head_w * 0.12), s(cy - head_h * 0.16)),
        (s(cx - head_w * 0.30), s(cy - head_h * 0.22)),
    ]
    draw.polygon(hair, fill=INK)

    # Brows: the single most recognisable line in the character, so they get
    # the thickest stroke on the card.
    for sign in (-1, 1):
        bx = cx + sign * head_w * 0.20
        draw.line(
            [(s(bx - sign * 26), s(cy - head_h * 0.055)),
             (s(bx + sign * 22), s(cy - head_h * 0.085))],
            fill=INK, width=int(s(7)),
        )
        draw.ellipse(
            (s(bx - 13), s(cy + head_h * 0.01), s(bx + 13), s(cy + head_h * 0.10)),
            fill=WHITE, outline=INK, width=int(s(2)),
        )
        draw.ellipse(
            (s(bx - 6), s(cy + head_h * 0.035), s(bx + 6), s(cy + head_h * 0.085)),
            fill=INK,
        )
        # Blush, low and far out, mostly hidden behind the cheek line.
        draw.ellipse(
            (s(cx + sign * head_w * 0.36 - 22), s(cy + head_h * 0.14),
             s(cx + sign * head_w * 0.36 + 22), s(cy + head_h * 0.22)),
            fill=BLUSH,
        )

    # Mouth: a short open line, no lips.
    draw.arc(
        (s(cx - 30), s(cy + head_h * 0.16), s(cx + 30), s(cy + head_h * 0.30)),
        start=15, end=165, fill=INK, width=int(s(3.2)),
    )

    # Shoulders in the red shirt, cut by the card edge.
    draw.rounded_rectangle(
        (s(cx - 132), s(cy + head_h * 0.42), s(cx + 132), s(height + 20)),
        radius=s(34), fill=RED, outline=INK, width=int(s(2.2)),
    )

    image = image.resize((width, height), Image.LANCZOS)
    return apply_grain(image, 5)


def recolor_texture() -> Image.Image:
    """Repaint the character's texture in the crayon palette.

    The atlas is a Live2D export: the mesh geometry decides the silhouette, so
    the only thing reachable from here is colour. Four ramps carry the whole
    image — paper white, ink, skin, and a warm highlight — and each is swapped
    for its counterpart by nearest-match so anti-aliased edges travel with the
    fill instead of being left behind as a halo.
    """
    source = Image.open(SOURCE / "cat.4096" / "texture_00.png").convert("RGBA")
    alpha = source.getchannel("A")

    # Quantise to a small palette, swap the palette, then rebuild. Doing the
    # swap on 16 entries instead of on 16.7 million pixels is the difference
    # between a second and a minute, and the source only ever used five ramps,
    # so nothing real is lost. Alpha rides through untouched: it carries the
    # cutouts, and quantising it would eat the edges.
    flat = source.convert("RGB").quantize(colors=16, method=Image.MEDIANCUT, dither=Image.NONE)
    palette = flat.getpalette()[: 16 * 3]

    # (from, to). Order matters: later entries win ties.
    swaps = [
        ((240, 240, 240), (253, 244, 226)),   # white body -> aged paper
        ((255, 255, 255), (253, 244, 226)),   # pure white -> aged paper
        ((240, 216, 192), (244, 158, 142)),   # soft highlight -> cheek blush
        ((240, 168, 120), (233, 132, 96)),    # orange fur -> crayon orange
        ((72, 96, 144), (58, 54, 50)),        # blue line art -> crayon ink
        ((120, 120, 120), (96, 92, 86)),      # grey shading -> softer ink
    ]

    swapped = bytearray(palette)
    for index in range(16):
        r, g, b = palette[index * 3 : index * 3 + 3]
        best, best_distance = None, 1 << 30
        for src, dst in swaps:
            distance = (r - src[0]) ** 2 + (g - src[1]) ** 2 + (b - src[2]) ** 2
            if distance < best_distance:
                best_distance, best = distance, dst
        if best is not None and best_distance < 9000:
            swapped[index * 3 : index * 3 + 3] = bytes(best)
        else:
            # A hand-mixed colour the artist used for one detail. Warm it toward
            # the palette without moving it far enough to change the shape.
            swapped[index * 3 : index * 3 + 3] = bytes(
                (
                    min(255, int(r * 0.94 + 10)),
                    min(255, int(g * 0.88 + 8)),
                    min(255, int(b * 0.82 + 6)),
                )
            )

    flat.putpalette(bytes(swapped))
    rebuilt = flat.convert("RGB").convert("RGBA")
    rebuilt.putalpha(alpha)
    return rebuilt


MODEL_JSON = {
    "Version": 3,
    "FileReferences": {
        "Moc": "cat.moc3",
        "Textures": ["cat.4096/texture_00.png"],
        "Physics": "cat.physics3.json",
        "DisplayInfo": "cat.cdi3.json",
        "Expressions": [
            {"Name": "live2d_expression0", "File": "live2d_expression0.exp3.json"},
            {"Name": "live2d_expression1", "File": "live2d_expression1.exp3.json"},
        ],
    },
    "Groups": [
        {"Target": "Parameter", "Name": "LipSync", "Ids": []},
        {"Target": "Parameter", "Name": "EyeBlink", "Ids": ["ParamEyeLOpen", "ParamEyeROpen"]},
    ],
    "HitAreas": [],
}


def main() -> None:
    MODEL.mkdir(parents=True, exist_ok=True)
    (MODEL / "cat.4096").mkdir(exist_ok=True)
    (MODEL / "resources" / "left-keys").mkdir(parents=True, exist_ok=True)
    (MODEL / "resources" / "right-keys").mkdir(parents=True, exist_ok=True)

    print("model3.json")
    (MODEL / "cat.model3.json").write_text(
        json.dumps(MODEL_JSON, ensure_ascii=False, indent="\t") + "\n",
        encoding="utf-8",
        newline="\n",
    )

    print("texture")
    texture = recolor_texture()
    texture.save(MODEL / "cat.4096" / "texture_00.png", optimize=True)

    print("background")
    build_background().save(MODEL / "resources" / "background.png", optimize=True)

    print("cover")
    build_cover().save(MODEL / "resources" / "cover.png", optimize=True)

    count = 0
    for name, side, col, row, span in LAYOUT:
        image = build_key_image(side, col, row, span)
        image.save(MODEL / "resources" / ("left-keys" if side == "left" else "right-keys") / f"{name}.png", optimize=True)
        count += 1
        if count % 10 == 0:
            print(f"  keys {count}/{len(LAYOUT)}")

    total = sum(p.stat().st_size for p in MODEL.rglob("*") if p.is_file())
    print(f"done: {count} key images, {total / 1024:.0f} KB on disk")


if __name__ == "__main__":
    main()
