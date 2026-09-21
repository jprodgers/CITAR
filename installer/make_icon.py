"""Draw the CITAR icon: a blue hexagon on transparency, marked "AI".

The same shape as the browser favicon, which is defined inline in ``citar/web/index.html`` — a
hexagon because the game is played on one, and the two letters because what makes CITAR different
from other Civ-likes is who is holding the controller.

    python installer/make_icon.py

Writes ``installer/citar.ico`` (the sizes Windows asks for, from the taskbar to the 256-pixel
"extra large icons" view) and ``docs/assets/icon.png`` for the documentation and the landing page.
Both are committed, so this only needs running when the design changes; Pillow is a development
dependency and is not needed to run or install CITAR.
"""
from __future__ import annotations

import math
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
HEX_FILL = (60, 120, 216, 255)          # the accent blue the web client uses
HEX_EDGE = (110, 165, 245, 255)
TEXT = (255, 255, 255, 255)

#: Windows picks from these. 16 and 32 are the taskbar and Explorer list; 256 is the large view,
#: and leaving it out makes an icon that looks fine until somebody switches view and sees mush.
SIZES = (16, 24, 32, 48, 64, 128, 256)


def hexagon(size: int) -> list[tuple[float, float]]:
    """A flat-topped hexagon inscribed in a square of *size*, matching the favicon's proportions."""
    centre = size / 2
    radius = size * 0.48
    return [
        (centre + radius * math.cos(math.radians(angle)),
         centre + radius * math.sin(math.radians(angle)))
        for angle in range(-90, 270, 60)
    ]


def font_for(size: int):
    """The largest bold sans-serif that fits, falling back to Pillow's own bitmap font.

    A build machine with no fonts installed is a normal CI runner, and an icon that is a blue
    hexagon with nothing on it is better than a build that fails.
    """
    for name in ("segoeuib.ttf", "arialbd.ttf", "DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf"):
        try:
            return ImageFont.truetype(name, int(size * 0.42))
        except OSError:
            continue
    return ImageFont.load_default()


def draw(size: int) -> Image.Image:
    # Drawn at four times the final size and scaled down: Pillow has no anti-aliasing for polygons,
    # so at 16 pixels a hexagon drawn directly has visibly ragged edges.
    scale = 4
    big = size * scale
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    canvas = ImageDraw.Draw(image)
    canvas.polygon(hexagon(big), fill=HEX_FILL, outline=HEX_EDGE, width=max(1, big // 64))

    # The two letters are dropped below 24 pixels: at that size they are three grey smudges, and a
    # clean silhouette reads better in a taskbar than an unreadable label.
    if size >= 24:
        font = font_for(big)
        text = "AI"
        left, top, right, bottom = canvas.textbbox((0, 0), text, font=font)
        canvas.text(((big - (right - left)) / 2 - left, (big - (bottom - top)) / 2 - top),
                    text, font=font, fill=TEXT)

    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    images = [draw(size) for size in SIZES]
    ico = ROOT / "installer" / "citar.ico"
    images[-1].save(ico, format="ICO", sizes=[(s, s) for s in SIZES])
    print(f"wrote {ico}")

    assets = ROOT / "docs" / "assets"
    assets.mkdir(parents=True, exist_ok=True)
    png = assets / "icon.png"
    draw(512).save(png, format="PNG")
    print(f"wrote {png}")


if __name__ == "__main__":
    main()
