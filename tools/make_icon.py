"""Generate Glimt icon assets: assets/glimt.ico (multi-size) and assets/tray.png.

Run from the repo root: python tools/make_icon.py
Requires Pillow (pip install pillow).
"""

import os

from PIL import Image, ImageDraw

MASTER = 1024  # render large, downsample for crisp edges
BG = (27, 30, 40, 255)  # #1B1E28
BOLT = (255, 197, 61, 255)  # #FFC53D
HIGHLIGHT = (255, 255, 255, 90)

# Classic 7-point lightning bolt in a 0..1 unit square.
BOLT_POINTS = [
    (0.62, 0.06),
    (0.28, 0.55),
    (0.47, 0.55),
    (0.38, 0.94),
    (0.72, 0.45),
    (0.53, 0.45),
    (0.62, 0.06),
]


def scale(points, size, inset=0.14):
    span = size * (1 - 2 * inset)
    off = size * inset
    return [(off + x * span, off + y * span) for x, y in points]


def draw_master(with_bg):
    img = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if with_bg:
        r = MASTER * 0.22
        d.rounded_rectangle([0, 0, MASTER - 1, MASTER - 1], radius=r, fill=BG)
    pts = scale(BOLT_POINTS, MASTER)
    # Subtle white highlight: same bolt nudged up-left, peeking out behind the amber one.
    shift = MASTER * 0.012
    d.polygon([(x - shift, y - shift) for x, y in pts], fill=HIGHLIGHT)
    d.polygon(pts, fill=BOLT)
    return img


def main():
    os.makedirs("assets", exist_ok=True)

    master = draw_master(with_bg=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    master.resize((256, 256), Image.LANCZOS).save(
        "assets/glimt.ico",
        format="ICO",
        sizes=[(s, s) for s in sizes],
    )

    tray = draw_master(with_bg=False).resize((32, 32), Image.LANCZOS)
    tray.save("assets/tray.png")
    print("Wrote assets/glimt.ico and assets/tray.png")


if __name__ == "__main__":
    main()
