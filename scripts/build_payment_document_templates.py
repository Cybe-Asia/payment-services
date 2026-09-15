"""Build privacy-safe PDF backgrounds from the approved Canva samples.

The source PDFs are flattened 300-DPI images. This script is intentionally kept
out of the runtime path: it removes all parent, student, document-number, and
financial values before producing the JPEG assets embedded by the Rust renderer.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw


PAGE_WIDTH = 595.44
PAGE_HEIGHT = 842.16
WHITE = (255, 255, 255)
ROW_SHADE = (247, 248, 246)
ACCENTS = {
    "iiss": (243, 152, 59),
    "iihs": (79, 115, 68),
}


def page_box(image: Image.Image, box: tuple[float, float, float, float]) -> tuple[int, int, int, int]:
    left, top, right, bottom = box
    return (
        round(left * image.width / PAGE_WIDTH),
        round(top * image.height / PAGE_HEIGHT),
        round(right * image.width / PAGE_WIDTH),
        round(bottom * image.height / PAGE_HEIGHT),
    )


def fill(draw: ImageDraw.ImageDraw, image: Image.Image, box: tuple[float, float, float, float], color: tuple[int, int, int]) -> None:
    draw.rectangle(page_box(image, box), fill=color)


def sanitize(source: Path, destination: Path, school: str, kind: str) -> None:
    image = Image.open(source).convert("RGB")
    draw = ImageDraw.Draw(image)
    accent = ACCENTS[school]

    # Parent/student identity, issue date, and document reference.
    fill(draw, image, (48, 145, 350, 169), WHITE)
    recipient_right = 358 if kind == "invoice" else 322
    fill(draw, image, (48, 169, recipient_right, 255), WHITE)
    fill(draw, image, (321, 145, 595.44, 169), WHITE)
    document_number_left = 358 if kind == "invoice" else 322
    fill(draw, image, (document_number_left, 222, 595.44, 255), accent)

    # Line items and totals. Recreate the original alternating Canva bands so
    # no source financial value remains in the embedded asset.
    bands = (
        (306, 337, WHITE),
        (337, 369, ROW_SHADE),
        (369, 401, WHITE),
        (401, 433, ROW_SHADE),
        (433, 454, WHITE),
        (454, 486, ROW_SHADE),
    )
    for top, bottom, color in bands:
        fill(draw, image, (0, top, 595.44, bottom), color)

    if kind == "invoice":
        fill(draw, image, (379, 505, 536, 540), accent)
        fill(draw, image, (50, 554, 310, 610), WHITE)
    else:
        fill(draw, image, (219, 505, 536, 540), accent)
        fill(draw, image, (50, 554, 286, 636), WHITE)
        fill(draw, image, (286, 554, 595.44, 636), WHITE)

    destination.parent.mkdir(parents=True, exist_ok=True)
    image.save(destination, format="JPEG", quality=92, optimize=True, progressive=False)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    for school in ("iiss", "iihs"):
        for kind in ("invoice", "receipt"):
            sanitize(
                args.source_dir / f"{school}-{kind}-000.ppm",
                args.output_dir / f"{school}-{kind}.jpg",
                school,
                kind,
            )


if __name__ == "__main__":
    main()
