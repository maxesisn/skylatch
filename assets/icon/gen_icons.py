#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Render the Skylatch "Carbon Precision" Lock-Bolt icon (see skylatch.svg) to PNG frames,
a PNG-in-ICO file and the macOS menu bar template image.

Pure standard library (zlib + struct), 4x4 supersampling per pixel, deterministic output:

    python assets/icon/gen_icons.py

Writes next to this script: skylatch.ico (16, 24, 32, 48, 64, 128, 256 px), skylatch-256.png,
skylatch-512.png (for the @2x macOS .icns) and skylatch-tray-template.png (36x36, black on
transparent, for NSImage isTemplate). Keep the shape table below in sync with skylatch.svg.
"""
import math
import pathlib
import struct
import zlib

SIZES = (16, 24, 32, 48, 64, 128, 256)
SS = 4  # supersampling factor per axis
TRAY_PX = 36

TILE_EDGE, TILE, PAPER, COBALT, BLACK = 1, 2, 3, 4, 5
PALETTE = {
    TILE_EDGE: (0x2E, 0x32, 0x3B, 255),
    TILE: (0x1B, 0x1D, 0x22, 255),
    PAPER: (0xF4, 0xF2, 0xEC, 255),
    COBALT: (0x3B, 0x82, 0xF6, 255),
    BLACK: (0, 0, 0, 255),
}

# Housing centreline in 64x64 viewBox units: ("L", ax, ay, bx, by) lines and
# ("A", cx, cy, r, sx, sy) quarter arcs lying in the quadrant (sx, sy) around the centre.
HOUSING = (
    ("L", 44, 22, 44, 18),
    ("A", 40, 18, 4, 1, -1),
    ("L", 40, 14, 18, 14),
    ("A", 18, 18, 4, -1, -1),
    ("L", 14, 18, 14, 46),
    ("A", 18, 46, 4, -1, 1),
    ("L", 18, 50, 40, 50),
    ("A", 40, 46, 4, 1, 1),
    ("L", 44, 46, 44, 42),
)


def rrect(x0, y0, x1, y1, r, colour):
    return ("rrect", (x0, y0, x1, y1, r), colour)


def shapes(housing_width, colours):
    """Painted in order; `colours` = (tile edge, tile, housing, bolt, handle) or None for no tile."""
    edge, tile, housing, bolt, handle = colours
    out = []
    if tile is not None:
        out += [
            rrect(3, 3, 61, 61, 15, edge),  # stroke 2 centred on the 56x56 rx14 rect
            rrect(5, 5, 59, 59, 13, tile),
        ]
    return out + [
        ("path", (HOUSING, housing_width / 2.0), housing),
        rrect(25, 27, 50, 37, 2.5, bolt),
        rrect(22, 21, 29.5, 43, 2.5, handle),
    ]


FULL = shapes(4, (TILE_EDGE, TILE, PAPER, COBALT, PAPER))
# Template: no tile, one colour, slightly heavier housing so it holds up at menu bar size.
TRAY = shapes(5, (None, None, BLACK, BLACK, BLACK))
TRAY_WINDOW = (8.0, 48.0)  # viewBox origin and span of the 36 px canvas (24-unit glyph box)


def inside_rrect(x, y, p):
    x0, y0, x1, y1, r = p
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    dx = max(x0 + r - x, x - (x1 - r), 0.0)
    dy = max(y0 + r - y, y - (y1 - r), 0.0)
    return dx * dx + dy * dy <= r * r


def inside_path(x, y, p):
    segments, half = p
    if x < 14 - half or x > 44 + half or y < 14 - half or y > 50 + half:
        return False
    limit = half * half
    for seg in segments:
        if seg[0] == "L":
            _, ax, ay, bx, by = seg
            vx, vy = bx - ax, by - ay
            t = ((x - ax) * vx + (y - ay) * vy) / (vx * vx + vy * vy)
            t = 0.0 if t < 0.0 else 1.0 if t > 1.0 else t
            dx, dy = x - (ax + t * vx), y - (ay + t * vy)
            if dx * dx + dy * dy <= limit:
                return True
        else:
            _, cx, cy, r, sx, sy = seg
            dx, dy = x - cx, y - cy
            if dx * sx >= 0 and dy * sy >= 0 and abs(math.hypot(dx, dy) - r) <= half:
                return True
    return False


TESTS = {"rrect": inside_rrect, "path": inside_path}


def render(size, shape_list=FULL, origin=0.0, span=64.0):
    """Return RGBA bytes for one frame covering viewBox [origin, origin + span]."""
    n = size * SS
    scale = n / span
    samples = []
    for j in range(n):
        y = origin + (j + 0.5) / scale
        row = []
        for i in range(n):
            x = origin + (i + 0.5) / scale
            c = 0
            for kind, params, colour in shape_list:
                if TESTS[kind](x, y, params):
                    c = colour
            row.append(c)
        samples.append(row)
    out = bytearray()
    area = SS * SS
    for py in range(size):
        block = samples[py * SS:(py + 1) * SS]
        for px in range(size):
            r = g = b = a = 0
            for row in block:
                for c in row[px * SS:(px + 1) * SS]:
                    if c:
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
    frames = {size: png(size, render(size)) for size in (*SIZES, 512)}
    (here / "skylatch.ico").write_bytes(ico([(size, frames[size]) for size in SIZES]))
    (here / "skylatch-256.png").write_bytes(frames[256])
    (here / "skylatch-512.png").write_bytes(frames[512])
    tray = png(TRAY_PX, render(TRAY_PX, TRAY, *TRAY_WINDOW))
    (here / "skylatch-tray-template.png").write_bytes(tray)
    print("wrote skylatch.ico (%s px), skylatch-256.png, skylatch-512.png, skylatch-tray-template.png"
          % ", ".join(str(s) for s in SIZES))


if __name__ == "__main__":
    main()
