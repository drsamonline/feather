#!/usr/bin/env python3
"""Compare a rendered page against its committed baseline.

The blank-frame check in CI only asks whether *something* was drawn. That is not
enough: a renderer can draw every glyph in the wrong place and still pass it, and
that is exactly what happened when the shaping migration shipped with periods
sitting at the top of the line and the dot of every i missing.

This compares ink. It is deliberately tolerant — font versions differ between
runner images, and a hard pixel match would fail for reasons that are not bugs —
but a broken draw path moves far more than a couple of percent.

  python3 tests/compare_render.py rendered.png baseline.png [tolerance]
"""

import sys

from PIL import Image

DARK = 160


def ink(image):
    """The coordinates of every pixel darker than mid-grey."""
    grey = image.convert("L")
    width, height = grey.size
    pixels = grey.load()
    return {
        (x, y)
        for y in range(height)
        for x in range(width)
        if pixels[x, y] < DARK
    }


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2

    rendered_path, baseline_path = sys.argv[1], sys.argv[2]
    tolerance = float(sys.argv[3]) if len(sys.argv) > 3 else 0.02

    rendered = Image.open(rendered_path).convert("RGB")
    baseline = Image.open(baseline_path).convert("RGB")

    if rendered.size != baseline.size:
        print(
            f"::error::the render is {rendered.size} and the baseline is "
            f"{baseline.size} — the page geometry changed"
        )
        return 1

    drawn = ink(rendered)
    expected = ink(baseline)

    if not expected:
        print("::error::the baseline has no ink; it cannot prove anything")
        return 1
    if not drawn:
        print("::error::the render has no ink at all")
        return 1

    differing = len(drawn ^ expected) / len(expected)
    print(
        f"ink: render {len(drawn)} px, baseline {len(expected)} px, "
        f"{differing * 100:.2f}% differs (tolerance {tolerance * 100:.0f}%)"
    )

    if differing > tolerance:
        print(
            "::error::the text no longer lands where it did — glyph placement or "
            "the face itself has changed"
        )
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
