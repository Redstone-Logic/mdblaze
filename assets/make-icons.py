#!/usr/bin/env python3
"""Build every icon file this program installs, from the two masters.

    python3 assets/make-icons.py

The mark is a rendered blaze with type set over it, so the source of truth is
raster: `icon-master.png` at 1024. There is no SVG to scale from, and pretending
otherwise by tracing it would lose the thing that makes it look like anything.

`icon-master-small.png` is the same tile carrying only an `m`. Below about 32
pixels there is no room for two letters and a flame — the letters close up and
the blaze becomes a smudge behind them — so the small sizes use it instead.
Showing less on purpose beats showing mush; it is the same reduction your eye
makes squinting at the full mark.

Only Pillow-free stdlib is used, so this runs anywhere Python does. Resampling is
a box filter over the premultiplied pixels, which is what keeps the flame's soft
edges from developing a dark halo.
"""
import os, struct, zlib

HERE = os.path.dirname(os.path.abspath(__file__))

# Where the mark stops being two letters. 32 keeps `md` legible; below it the
# letters touch.
SMALL_BELOW = 32

# What each container carries. Linux wants a directory per size, Windows one
# file with several, macOS one file with several under typed chunks.
LINUX_SIZES = (16, 22, 24, 32, 48, 64, 128, 256, 512)
ICO_SIZES = (16, 32, 48, 256)
ICNS_TYPES = ((b"icp4", 16), (b"icp5", 32), (b"ic11", 32),
              (b"ic07", 128), (b"ic08", 256), (b"ic09", 512))


def read_png(path):
    """Minimal PNG reader: 8-bit RGBA, non-interlaced, which is what we write."""
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    pos, idat, w = 8, b"", None
    while pos < len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        tag = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + n]
        if tag == b"IHDR":
            w, h, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert (depth, colour, interlace) == (8, 6, 0), f"{path}: want 8-bit RGBA"
        elif tag == b"IDAT":
            idat += body
        pos += 12 + n
    raw = zlib.decompress(idat)
    # Undo the per-scanline filters.
    out, stride, prev = bytearray(), w * 4, bytearray(w * 4)
    at = 0
    for _ in range(h):
        f = raw[at]; at += 1
        line = bytearray(raw[at:at + stride]); at += stride
        for i in range(stride):
            a = line[i - 4] if i >= 4 else 0
            b = prev[i]
            c = prev[i - 4] if i >= 4 else 0
            if f == 1: line[i] = (line[i] + a) & 0xFF
            elif f == 2: line[i] = (line[i] + b) & 0xFF
            elif f == 3: line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        out += line
        prev = line
    return w, h, bytes(out)


def resize(w, h, px, n):
    """Box filter, averaging in PREMULTIPLIED space.

    Straight-alpha averaging mixes the colour of transparent pixels into opaque
    ones, which puts a dark halo around every soft edge -- and this mark is
    almost entirely soft edges.
    """
    out = bytearray(n * n * 4)
    for y in range(n):
        y0, y1 = y * h // n, max((y + 1) * h // n, y * h // n + 1)
        for x in range(n):
            x0, x1 = x * w // n, max((x + 1) * w // n, x * w // n + 1)
            r = g = b = a = c = 0
            for sy in range(y0, min(y1, h)):
                row = sy * w * 4
                for sx in range(x0, min(x1, w)):
                    i = row + sx * 4
                    pa = px[i + 3]
                    r += px[i] * pa; g += px[i + 1] * pa; b += px[i + 2] * pa
                    a += pa; c += 1
            o = (y * n + x) * 4
            if c == 0 or a == 0:
                continue
            out[o] = min(255, r // a); out[o + 1] = min(255, g // a)
            out[o + 2] = min(255, b // a); out[o + 3] = a // c
    return bytes(out)


def png(n, rgba):
    rows = b"".join(b"\x00" + rgba[y * n * 4:(y + 1) * n * 4] for y in range(n))

    def chunk(tag, d):
        c = tag + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))


def main():
    big = read_png(os.path.join(HERE, "icon-master.png"))
    small = read_png(os.path.join(HERE, "icon-master-small.png"))
    want = sorted(set(LINUX_SIZES) | set(ICO_SIZES) | {s for _, s in ICNS_TYPES} | {512})
    at = {}
    for s in want:
        w, h, px = small if s < SMALL_BELOW else big
        at[s] = png(s, resize(w, h, px, s))

    out = os.path.join(HERE, "icons")
    os.makedirs(out, exist_ok=True)
    for s in LINUX_SIZES:
        open(os.path.join(out, f"{s}.png"), "wb").write(at[s])

    head = struct.pack("<HHH", 0, 1, len(ICO_SIZES))
    offset = 6 + 16 * len(ICO_SIZES)
    entries = blobs = b""
    for s in ICO_SIZES:
        d = at[s]
        entries += struct.pack("<BBBBHHII", 0 if s == 256 else s, 0 if s == 256 else s,
                               0, 0, 1, 32, len(d), offset)
        blobs += d
        offset += len(d)
    open(os.path.join(HERE, "icon.ico"), "wb").write(head + entries + blobs)

    body = b"".join(t + struct.pack(">I", len(at[s]) + 8) + at[s] for t, s in ICNS_TYPES)
    open(os.path.join(HERE, "icon.icns"), "wb").write(
        b"icns" + struct.pack(">I", len(body) + 8) + body)

    open(os.path.join(HERE, "icon.png"), "wb").write(at[512])
    print(f"wrote icons/ ({len(LINUX_SIZES)} sizes), icon.ico, icon.icns, icon.png")


if __name__ == "__main__":
    main()
