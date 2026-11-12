#!/usr/bin/env python3
"""AROS Workbench icons (PNG .info) from WordCraft's own 64x64 app icon.

An AROS PNG icon is a normal PNG with an `icOn` chunk of (tag, value) pairs,
followed by a second PNG for the selected state (AROS
workbench/libs/icon/diskobjPNGio.c). Tags used here:
  0x8000100f  icon type (2 = drawer, 3 = tool)
  0x80001009  stack size in bytes (a tool's stack when started from Workbench)
  0x8000100b  one ToolType, as a NUL-terminated string (repeated per ToolType)

Usage: icons.py SOURCE.png DEST_DIR NAME STACK
writes DEST_DIR/NAME/NAME.info (tool) and DEST_DIR/NAME.info (drawer).
package.py calls icon() directly to add the tool's ToolTypes.
"""
import struct
import sys
import zlib
from pathlib import Path

ICON_DRAWER, ICON_TOOL = 2, 3


def chunk(name, data):
    return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data) & 0xFFFFFFFF)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else b if pb <= pc else c


def decode_rgba(path):
    """Pixels of an 8-bit RGBA, non-interlaced PNG as rows of bytes."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos, idat, header = 8, bytearray(), None
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        name, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if name == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif name == b"IDAT":
            idat += body
        elif name == b"IEND":
            break
    width, height, depth, colour, _, _, interlace = header
    if (depth, colour, interlace) != (8, 6, 0):
        raise SystemExit(f"{path}: need 8-bit RGBA non-interlaced, got depth {depth} colour {colour} interlace {interlace}")
    raw, stride, bpp = zlib.decompress(bytes(idat)), width * 4, 4
    rows, prev, off = [], bytearray(stride), 0
    for _ in range(height):
        kind, row = raw[off], bytearray(raw[off + 1:off + 1 + stride])
        off += 1 + stride
        for x in range(stride):
            left = row[x - bpp] if x >= bpp else 0
            up_left = prev[x - bpp] if x >= bpp else 0
            row[x] = (row[x] + (0, left, prev[x], (left + prev[x]) // 2, paeth(left, prev[x], up_left))[kind]) & 255
        rows.append(bytes(row))
        prev = row
    return width, height, rows


def encode(width, height, rows, tags):
    raw = b"".join(b"\0" + r for r in rows)
    meta = b"".join(struct.pack(">II", t, v) for t, v in tags)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"icOn", meta) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def selected(rows):
    # Darkened colours, alpha kept: the selected state reads as pressed on light and dark backdrops.
    out = []
    for r in rows:
        b = bytearray(r)
        for i in range(0, len(b), 4):
            b[i], b[i + 1], b[i + 2] = (b[i] * 3) // 5, (b[i + 1] * 3) // 5, (b[i + 2] * 3) // 5
        out.append(bytes(b))
    return out


def icon(width, height, rows, kind, stack, tooltypes=()):
    tags = [(0x8000100F, kind)] + ([(0x80001009, stack)] if kind == ICON_TOOL else [])
    def state(pixels):
        data = encode(width, height, pixels, tags)
        pos = data.index(b"icOn") - 4
        length = struct.unpack(">I", data[pos:pos+4])[0]
        meta = data[pos+8:pos+8+length]
        meta += b"".join(struct.pack(">I", 0x8000100B) + t.encode("ascii") + b"\0" for t in tooltypes)
        return data[:pos] + chunk(b"icOn", meta) + data[pos+12+length:]
    return state(rows) + state(selected(rows))


def main():
    source, dest, name, stack = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], int(sys.argv[4])
    width, height, rows = decode_rgba(source)
    (dest / name).mkdir(parents=True, exist_ok=True)
    (dest / name / f"{name}.info").write_bytes(icon(width, height, rows, ICON_TOOL, stack))
    (dest / f"{name}.info").write_bytes(icon(width, height, rows, ICON_DRAWER, stack))


if __name__ == "__main__":
    main()
