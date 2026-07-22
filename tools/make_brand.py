"""Generate Glimt brand assets from one mark definition.

Outputs:
  assets/glimt.ico          multi-size app icon (brackets dropped below 32 px)
  assets/tray.png           32 px tray icon
  assets/tray-rec.png       32 px tray icon with recording dot
  screenshots/social-banner.png  1280x640 GitHub social preview / README banner

Run from the repo root: python tools/make_brand.py
Requires Pillow (pip install pillow).
"""

import math
import os

from PIL import Image, ImageDraw, ImageFont

INK = (27, 30, 40, 255)  # #1B1E28
AMBER = (255, 197, 61, 255)  # #FFC53D
RED = (229, 72, 77, 255)  # #E5484D
SLATE = (139, 147, 167, 255)  # #8B93A7
PAPER = (242, 244, 248, 255)  # #F2F4F8

MASTER = 1024  # render large, downsample for crisp edges


def astroid(cx, cy, r, n=240):
    """Four-point light glint: astroid curve, cusps at N/E/S/W."""
    pts = []
    for i in range(n):
        t = 2 * math.pi * i / n
        pts.append((cx + r * math.cos(t) ** 3, cy + r * math.sin(t) ** 3))
    return pts


def bracket_corner(d, cx, cy, dx, dy, arm, thick, color):
    """One L bracket with its outer corner exactly at (cx, cy), arms running dx/dy inward.

    Both arm rects start at the corner so their roundings overlap into one clean
    rounded outer corner; offsetting them leaves a notch at the tip.
    """
    r = thick / 2
    x0, x1 = sorted([cx, cx + dx * arm])
    y0, y1 = sorted([cy, cy + dy * thick])
    d.rounded_rectangle([x0, y0, x1, y1], radius=r, fill=color)
    x0, x1 = sorted([cx, cx + dx * thick])
    y0, y1 = sorted([cy, cy + dy * arm])
    d.rounded_rectangle([x0, y0, x1, y1], radius=r, fill=color)


def brackets(d, size, inset, arm, thick, color):
    """Viewfinder corner brackets framing a square canvas."""
    lo, hi = inset, size - inset
    for cx, cy, dx, dy in [(lo, lo, 1, 1), (hi, lo, -1, 1), (lo, hi, 1, -1), (hi, hi, -1, -1)]:
        bracket_corner(d, cx, cy, dx, dy, arm, thick, color)


def mark(size=MASTER, with_brackets=True):
    """The Glimt mark: amber tile, ink capture brackets, ink glint."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=size * 0.22, fill=AMBER)
    if with_brackets:
        brackets(d, size, size * 0.13, size * 0.14, size * 0.055, INK)
        d.polygon(astroid(size * 0.5, size * 0.5, size * 0.28), fill=INK)
    else:
        # small sizes: brackets turn to mush, so the glint carries the mark alone
        d.polygon(astroid(size * 0.5, size * 0.5, size * 0.34), fill=INK)
    return img


def make_ico():
    full = mark()
    simple = mark(with_brackets=False)
    frames = [full.resize((s, s), Image.LANCZOS) for s in (256, 128, 64, 48, 32)]
    frames += [simple.resize((s, s), Image.LANCZOS) for s in (24, 16)]
    frames[0].save("assets/glimt.ico", format="ICO", append_images=frames[1:])


def make_tray():
    mark().resize((32, 32), Image.LANCZOS).save("assets/tray.png")
    # recording variant: red dot over the glint, centered in the lower-right quadrant
    rec = mark()
    d = ImageDraw.Draw(rec)
    cx = cy = MASTER * 0.75
    r = MASTER * 0.28
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=RED)
    rec.resize((32, 32), Image.LANCZOS).save("assets/tray-rec.png")


def make_banner():
    w, h = 1280, 640
    img = Image.new("RGBA", (w, h), INK)
    d = ImageDraw.Draw(img)

    # the banner is itself a Glimt selection, framed by faint corner brackets
    dim_slate = (139, 147, 167, 110)
    inset, arm = 36, 52
    for cx, cy, dx, dy in [(inset, inset, 1, 1), (w - inset, inset, -1, 1),
                           (inset, h - inset, 1, -1), (w - inset, h - inset, -1, -1)]:
        bracket_corner(d, cx, cy, dx, dy, arm, 7, dim_slate)

    font_big = ImageFont.truetype("assets/Inter-Medium.ttf", 148)
    font_small = ImageFont.truetype("assets/Inter-Medium.ttf", 34)

    tile = mark().resize((240, 240), Image.LANCZOS)
    name = "Glimt"
    tagline = "Instant area screenshots for Windows"

    name_w = d.textlength(name, font=font_big)
    tag_w = d.textlength(tagline, font=font_small)
    gap = 64
    total_w = 240 + gap + max(name_w, tag_w)
    x0 = (w - total_w) / 2
    ty = (h - 240) / 2
    img.alpha_composite(tile, (int(x0), int(ty)))
    tx = x0 + 240 + gap
    d.text((tx, h / 2 - 118), name, font=font_big, fill=PAPER)
    d.text((tx + 6, h / 2 + 44), tagline, font=font_small, fill=SLATE)

    os.makedirs("screenshots", exist_ok=True)
    img.convert("RGB").save("screenshots/social-banner.png")


def main():
    os.makedirs("assets", exist_ok=True)
    make_ico()
    make_tray()
    make_banner()
    print("Wrote assets/glimt.ico, assets/tray.png, assets/tray-rec.png, screenshots/social-banner.png")


if __name__ == "__main__":
    main()
