#!/usr/bin/env python3
"""Generate a 64x64 checker-pattern PNG using only the Python stdlib.

Output: assets/images/smoke.png
Colors: blue (#1e40af) × amber (#f59e0b) — chosen so the pattern is obviously
distinct from the 50% gray placeholder rendered on load miss.

Self-contained: builds the PNG with zlib + struct only (no PIL, no Pillow).
Each checker cell is 8x8 px (64 / 8 = 8 cells per axis). Alpha = 0xFF.

Reference: PNG spec https://www.w3.org/TR/png/
"""

import struct
import zlib
from pathlib import Path


def make_checker_rgba(size: int, cell: int, color_a: bytes, color_b: bytes) -> bytes:
    """Return raw RGBA8 bytes (size*size*4) arranged as a checker pattern."""
    rows = bytearray()
    for y in range(size):
        row = bytearray()
        row.append(0)  # PNG filter byte for this row (0 = None)
        for x in range(size):
            cx = x // cell
            cy = y // cell
            row.extend(color_a if (cx + cy) % 2 == 0 else color_b)
        rows.extend(row)
    return bytes(rows)


def png_chunk(tag: bytes, data: bytes) -> bytes:
    length = struct.pack(">I", len(data))
    crc = struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    return length + tag + data + crc


def encode_png(pixel_rows: bytes, width: int, height: int) -> bytes:
    sig = b"\x89PNG\r\n\x1a\n"
    # IHDR: width, height, bit_depth=8, color_type=6 (RGBA),
    # compression=0, filter=0, interlace=0
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    idat = zlib.compress(pixel_rows, level=9)
    return (
        sig
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", idat)
        + png_chunk(b"IEND", b"")
    )


def main() -> None:
    size = 64
    cell = 8
    # #1e40af (blue-800) and #f59e0b (amber-500) — tailwindcss palette.
    color_a = bytes((0x1E, 0x40, 0xAF, 0xFF))
    color_b = bytes((0xF5, 0x9E, 0x0B, 0xFF))

    rows = make_checker_rgba(size, cell, color_a, color_b)
    png_bytes = encode_png(rows, size, size)

    out = Path(__file__).resolve().parent / "smoke.png"
    out.write_bytes(png_bytes)
    print(f"wrote {out} ({len(png_bytes)} bytes)")


if __name__ == "__main__":
    main()
