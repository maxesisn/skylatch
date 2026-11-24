#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Render the Skylatch Lock-Bolt icon (see skylatch.svg) to PNG frames and a PNG-in-ICO file.

Pure standard library (zlib + struct), 4x4 supersampling per pixel, deterministic output:

    python assets/icon/gen_icons.py

Writes skylatch.ico (16, 24, 32, 48, 64, 128, 256 px) and skylatch-256.png next to this script.
Keep the shape table below in sync with skylatch.svg.
"""
import math
import pathlib
import struct
import zlib

SIZES = (16, 24, 32, 48, 64, 128, 256)
SS = 4  # supersampling factor per axis

TRANSPARENT, INK, PAPER, COBALT = 0, 1, 2, 3
PALETTE = {
    TRANSPARENT: (0, 0, 0, 0),
    INK: (0x1A, 0x1A, 0x18, 255),
    PAPER: (0xFC, 0xFB, 0xF8, 255),
    COBALT: (0x25, 0x63, 0xEB, 255),
}

# (x0, y0, x1, y1, corner radius, colour) in 64x64 viewBox units, painted in order.
SHAPES = (
    (1.5, 1.5, 62.5, 62.5, 15.5, INK),    # tile outer edge: stroke 3 centred on a 58x58 rx14 rect
    (4.5, 4.5, 59.5, 59.5, 12.5, PAPER),  # tile fill
    (14, 14, 50, 50, 0, INK),             # housing outer
    (18, 18, 46, 46, 0, PAPER),           # housing inner
    (24, 29, 48, 35, 0, COBALT),          # bolt, tip inside the right wall (46..50)
    (20, 24, 24, 40, 0, INK),             # latch handle
)


def span(shape, y):
    """Horizontal extent [xa, xb] of a shape on scanline y (viewBox units), or None."""
    x0, y0, x1, y1, r, _ = shape
    if y < y0 or y > y1:
        return None
    inset = 0.0
    if r > 0:
        dy = max(y0 + r - y, y - (y1 - r), 0.0)
        if dy > 0:
            inset = r - math.sqrt(max(r * r - dy * dy, 0.0))
    return x0 + inset, x1 - inset


def render(size):
    """Return RGBA bytes for one frame."""
    n = size * SS
    scale = n / 64.0
    rows = []
    for j in range(n):
        y = (j + 0.5) / scale
        row = [TRANSPARENT] * n
        for shape in SHAPES:
            s = span(shape, y)
            if s is None:
                continue
            a = max(0, math.ceil(s[0] * scale - 0.5))
            b = min(n - 1, math.floor(s[1] * scale - 0.5))
            if b >= a:
                row[a:b + 1] = [shape[5]] * (b - a + 1)
        rows.append(row)
    out = bytearray()
    area = SS * SS
    for py in range(size):
        block = rows[py * SS:(py + 1) * SS]
        for px in range(size):
            r = g = b = a = 0
            for row in block:
                for c in row[px * SS:(px + 1) * SS]:
                    cr, cg, cb, ca = PALETTE[c]
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            if a == 0:
                out += b"\0\0\0\0"
            else:  # un-premultiply the averaged colour
                out += bytes((round(r / a), round(g / a), round(b / a), round(a / area)))
    return bytes(out)


def png(size, rgba):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\0" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def ico(frames):
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    entries = b""
    for size, data in frames:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return header + entries + b"".join(data for _, data in frames)


def main():
    here = pathlib.Path(__file__).resolve().parent
    frames = [(size, png(size, render(size))) for size in SIZES]
    (here / "skylatch.ico").write_bytes(ico(frames))
    (here / "skylatch-256.png").write_bytes(dict(frames)[256])
    print("wrote skylatch.ico (%s px) and skylatch-256.png" % ", ".join(str(s) for s in SIZES))


if __name__ == "__main__":
    main()
