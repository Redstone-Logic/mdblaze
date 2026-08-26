#!/usr/bin/env python3
"""Build every icon this program installs, from the shapes in `icon.svg`.

No dependencies and no drawing program. The shapes are a rounded square with a
vertical gradient and three round-capped polylines, all of which are a few lines
of arithmetic -- and rasterising them here means the icons can be rebuilt on any
machine with a Python, rather than on one that happens to have a particular
SVG renderer installed.

Sizes 48 and up are rendered from the vector geometry. 16 and 32 are hand-placed
pixels: see assets/README.md for why.

    python3 assets/make-icons.py
"""

import math, os, struct, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
SS = 4                      # supersampling for the vector sizes
STROKE = 6.0                # stroke width, in the 64-unit viewBox
RADIUS = 13.0               # corner radius, same units
TOP, BOT = (0xd1, 0x54, 0x4b), (0xa0, 0x2f, 0x28)
MARK = (0xfb, 0xf3, 0xf2)

# The same three paths as icon.svg, and they must stay the same.
PATHS = [
    [(13, 45), (13, 21), (22.5, 33), (32, 21), (32, 45)],   # M
    [(46, 20), (46, 39)],                                    # arrow shaft
    [(38.5, 33), (46, 43), (53.5, 33)],                      # arrowhead
]

# 16px carries the M alone: both glyphs at this size anti-alias into each other
# and the arrowhead vanishes.
M16 = [
    "XX......XX",
    "XX......XX",
    "XXX....XXX",
    "XXXX..XXXX",
    "XX.XXXX.XX",
    "XX..XX..XX",
    "XX......XX",
    "XX......XX",
]
M32 = [
    "XXX.......XXX...XXX...",
    "XXX.......XXX...XXX...",
    "XXXX.....XXXX...XXX...",
    "XXXXX...XXXXX...XXX...",
    "XXX.XX.XX.XXX...XXX...",
    "XXX..XXX..XXX...XXX...",
    "XXX...X...XXX.XXXXXXX.",
    "XXX.......XXX..XXXXX..",
    "XXX.......XXX...XXX...",
    "XXX.......XXX....X....",
]


def _seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    L = dx * dx + dy * dy
    t = 0.0 if L == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / L))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def render_vector(size):
    n = size * SS
    k = 64.0 / n
    acc = [[0, 0, 0, 0] for _ in range(size * size)]
    for yy in range(n):
        for xx in range(n):
            x, y = (xx + 0.5) * k, (yy + 0.5) * k
            cx = min(max(x, RADIUS), 64 - RADIUS)
            cy = min(max(y, RADIUS), 64 - RADIUS)
            if (x - cx) ** 2 + (y - cy) ** 2 > RADIUS * RADIUS:
                continue
            t = y / 64.0
            col = [int(TOP[i] + (BOT[i] - TOP[i]) * t) for i in range(3)]
            for path in PATHS:
                if any(_seg_dist(x, y, *path[i], *path[i + 1]) <= STROKE / 2.0
                       for i in range(len(path) - 1)):
                    col = list(MARK)
                    break
            o = (yy // SS) * size + (xx // SS)
            acc[o][0] += col[0]; acc[o][1] += col[1]; acc[o][2] += col[2]; acc[o][3] += 255
    out = bytearray()
    per = SS * SS
    for a in acc:
        alpha = a[3] // per
        if alpha == 0:
            out += bytes(4)
        else:
            out += bytes([a[0] * 255 // a[3], a[1] * 255 // a[3], a[2] * 255 // a[3], alpha])
    return bytes(out)


def render_pixels(size, art, radius):
    w, h = len(art[0]), len(art)
    ox, oy = (size - w) // 2, (size - h) // 2
    cells = {(ox + c, oy + r) for r, row in enumerate(art)
             for c, ch in enumerate(row) if ch != "."}
    out = bytearray()
    for y in range(size):
        for x in range(size):
            cx = min(max(x + 0.5, radius), size - radius)
            cy = min(max(y + 0.5, radius), size - radius)
            if (x + 0.5 - cx) ** 2 + (y + 0.5 - cy) ** 2 > radius * radius:
                out += bytes(4)
            elif (x, y) in cells:
                out += bytes([*MARK, 255])
            else:
                t = y / (size - 1)
                out += bytes([int(TOP[i] + (BOT[i] - TOP[i]) * t) for i in range(3)] + [255])
    return bytes(out)


def png(size, rgba):
    rows = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9))
            + chunk(b"IEND", b""))


def main():
    sizes = {
        16: png(16, render_pixels(16, M16, 3.0)),
        32: png(32, render_pixels(32, M32, 6.0)),
    }
    for s in (48, 128, 256, 512):
        sizes[s] = png(s, render_vector(s))

    # ICO: header, a directory entry per image, then the PNG bytes. A size of
    # 256 is written as 0, which is how the format says "256".
    order = [16, 32, 48, 256]
    head = struct.pack("<HHH", 0, 1, len(order))
    offset = 6 + 16 * len(order)
    entries, blobs = b"", b""
    for s in order:
        d = sizes[s]
        entries += struct.pack("<BBBBHHII", 0 if s == 256 else s, 0 if s == 256 else s,
                               0, 0, 1, 32, len(d), offset)
        blobs += d
        offset += len(d)
    open(os.path.join(HERE, "icon.ico"), "wb").write(head + entries + blobs)

    # ICNS: magic, total length, then typed chunks. These four types all take a
    # PNG payload and cover what the Finder asks for.
    types = [(b"ic11", 32), (b"ic07", 128), (b"ic08", 256), (b"ic09", 512)]
    body = b"".join(t + struct.pack(">I", len(sizes[s]) + 8) + sizes[s] for t, s in types)
    open(os.path.join(HERE, "icon.icns"), "wb").write(
        b"icns" + struct.pack(">I", len(body) + 8) + body)

    open(os.path.join(HERE, "icon.png"), "wb").write(sizes[512])
    print("wrote icon.ico, icon.icns, icon.png")


if __name__ == "__main__":
    main()
