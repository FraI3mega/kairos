#!/usr/bin/env python3
"""Renders src/icons/icon-<size>.png from the 9x9 pixel mark in crates/server/src/pages.rs.

White mark on the Kairos field colour, drawn with whole-pixel blocks so the edges stay crisp.
'#' cells are solid white, 'o' cells are white at the same 42% the website draws them.
"""
import re
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIELD = (0x3A, 0x33, 0xA3)
SIZES = (16, 32, 48, 128)
SOFT = 0.42


def read_mark():
    source = (ROOT.parent / "crates/server/src/pages.rs").read_text()
    found = re.search(r"const MARK: \[&str; 9\] = \[(.*?)\];", source)
    if not found:
        raise SystemExit("MARK not found in crates/server/src/pages.rs")
    rows = re.findall(r'"([.o#]{9})"', found.group(1))
    if len(rows) != 9:
        raise SystemExit("MARK should have 9 rows of 9 cells")
    return rows


def blend(alpha):
    return tuple(round(f + (255 - f) * alpha) for f in FIELD)


def render(mark, size):
    scale = max(1, (size * 3 // 4) // 9)
    offset = (size - 9 * scale) // 2
    colours = {"#": blend(1.0), "o": blend(SOFT), ".": FIELD}
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            mx, my = (x - offset) // scale, (y - offset) // scale
            inside = x >= offset and y >= offset and 0 <= mx < 9 and 0 <= my < 9
            row += bytes(colours[mark[my][mx]] if inside else FIELD)
        rows.append(bytes(row))
    return rows


def png(rows, size):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")


def main():
    mark = read_mark()
    out = ROOT / "src" / "icons"
    out.mkdir(parents=True, exist_ok=True)
    for size in SIZES:
        (out / f"icon-{size}.png").write_bytes(png(render(mark, size), size))


if __name__ == "__main__":
    main()
