#!/usr/bin/env python3
"""Generates the application icons from one description, in one place.

Velopack needs an icon per platform and will not build without one: an `.ico`
for the Windows installer and shortcuts, a PNG for the Linux AppImage. The
macOS bundle wants an `.icns`. Keeping all three as generated output rather
than three hand-drawn files means they cannot drift apart, and re-cutting them
after a design change is one command.

The mark is deliberately plain: the workbench's command accent, and the
isometric cube outline that the viewport draws on first run. It is a
placeholder for a designed icon, not a substitute for one.

    python3 scripts/build-icons.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

# The light palette's command accent, from crates/ui-core/src/theme.rs.
ACCENT = (18, 102, 189, 255)
MARK = (255, 255, 255, 255)

# Everything is drawn oversampled and then reduced, which is what gives the
# diagonals clean edges at the 16 px the Windows taskbar actually draws.
SUPERSAMPLE = 4
SIZE = 1024
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
ICNS_SIZES = (16, 32, 64, 128, 256, 512, 1024)

REPOSITORY = Path(__file__).resolve().parent.parent
ICON_DIRECTORY = REPOSITORY / "packaging" / "icons"


def draw_icon() -> Image.Image:
    """The rounded tile and the isometric cube outline, oversampled."""
    canvas = SIZE * SUPERSAMPLE
    image = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(
        [(0, 0), (canvas - 1, canvas - 1)],
        radius=int(canvas * 0.18),
        fill=ACCENT,
    )

    centre = canvas / 2
    half_height = canvas * 0.30
    half_width = half_height * 0.866  # A true isometric hexagon, not a guess.
    stroke = max(1, int(canvas * 0.030))

    def point(x: float, y: float) -> tuple[float, float]:
        return (centre + x, centre + y)

    top = point(0, -half_height)
    upper_left = point(-half_width, -half_height / 2)
    upper_right = point(half_width, -half_height / 2)
    lower_left = point(-half_width, half_height / 2)
    lower_right = point(half_width, half_height / 2)
    bottom = point(0, half_height)
    near = point(0, 0)

    outline = [top, upper_right, lower_right, bottom, lower_left, upper_left, top]
    draw.line(outline, fill=MARK, width=stroke, joint="curve")
    # The three edges meeting at the near corner: what makes a hexagon read as
    # a cube rather than a badge.
    for corner in (upper_left, upper_right, bottom):
        draw.line([near, corner], fill=MARK, width=stroke)
    # Line joins are not rounded, so the corners are capped by hand.
    for x, y in (*outline, near):
        radius = stroke / 2
        draw.ellipse([x - radius, y - radius, x + radius, y + radius], fill=MARK)

    return image.resize((SIZE, SIZE), Image.LANCZOS)


def write_icns(icon: Image.Image, destination: Path) -> bool:
    """Builds the macOS icon, if this is a macOS machine. `iconutil` ships with
    the developer tools and has no cross-platform equivalent, so on Linux and
    Windows the committed `.icns` is simply left as it is."""
    if not shutil.which("iconutil"):
        return False
    iconset = destination.with_suffix(".iconset")
    shutil.rmtree(iconset, ignore_errors=True)
    iconset.mkdir(parents=True)
    for size in ICNS_SIZES:
        icon.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
        if size * 2 <= SIZE:
            icon.resize((size * 2, size * 2), Image.LANCZOS).save(
                iconset / f"icon_{size}x{size}@2x.png"
            )
    subprocess.run(
        ["iconutil", "--convert", "icns", "--output", str(destination), str(iconset)],
        check=True,
    )
    shutil.rmtree(iconset, ignore_errors=True)
    return True


def main() -> int:
    ICON_DIRECTORY.mkdir(parents=True, exist_ok=True)
    icon = draw_icon()

    png = ICON_DIRECTORY / "artificer.png"
    icon.resize((512, 512), Image.LANCZOS).save(png)

    ico = ICON_DIRECTORY / "artificer.ico"
    icon.save(ico, sizes=[(size, size) for size in ICO_SIZES])

    written = [png, ico]
    icns = ICON_DIRECTORY / "artificer.icns"
    if write_icns(icon, icns):
        written.append(icns)
    else:
        print("iconutil is unavailable; leaving artificer.icns as it is")

    for path in written:
        print(f"wrote {path.relative_to(REPOSITORY)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
