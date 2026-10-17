"""Generate the OI Linux application icon set.

Pure standard library on purpose: the icon has to be reproducible on any build machine without
adding an image dependency to the toolchain. The artwork is a terminal prompt (`>_`) on a dark
rounded square, drawn with 4x supersampling so it stays crisp at 16 px.
"""

import os
import struct
import zlib

ICON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src-tauri", "icons")

BACKGROUND = (31, 36, 48, 255)
BORDER = (58, 68, 92, 255)
PROMPT = (158, 206, 106, 255)
CURSOR = (227, 231, 238, 255)


def rounded_square_coverage(x, y, size, radius, margin):
    """1.0 inside the rounded square, 0.0 outside, with a soft border."""
    left = margin
    right = size - margin
    if x < left or x > right or y < left or y > right:
        return 0.0, 0.0
    cx = min(max(x, left + radius), right - radius)
    cy = min(max(y, left + radius), right - radius)
    dx = x - cx
    dy = y - cy
    distance = (dx * dx + dy * dy) ** 0.5
    if distance <= radius - 1.0:
        return 1.0, 0.0
    if distance >= radius:
        return 0.0, 0.0
    inner = distance / radius
    return 1.0 - inner, 1.0


def distance_to_segment(px, py, ax, ay, bx, by):
    vx = bx - ax
    vy = by - ay
    wx = px - ax
    wy = py - ay
    length_sq = vx * vx + vy * vy
    t = 0.0 if length_sq == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / length_sq))
    dx = px - (ax + t * vx)
    dy = py - (ay + t * vy)
    return (dx * dx + dy * dy) ** 0.5


def render(size):
    margin = size * 0.055
    radius = size * 0.22
    thickness = size * 0.085
    ss = 4
    rows = []
    for y in range(size):
        row = []
        for x in range(size):
            r = g = b = a = 0.0
            for sy in range(ss):
                for sx in range(ss):
                    px = x + (sx + 0.5) / ss
                    py = y + (sy + 0.5) / ss
                    fill, border = rounded_square_coverage(px, py, size, radius, margin)
                    if fill <= 0.0:
                        continue
                    colour = BACKGROUND
                    edge = border * fill
                    if edge > 0:
                        colour = tuple(
                            int(BACKGROUND[i] * (1 - edge) + BORDER[i] * edge) for i in range(3)
                        ) + (255,)
                    # `>` chevron
                    d1 = distance_to_segment(
                        px, py, size * 0.30, size * 0.30, size * 0.47, size * 0.50
                    )
                    d2 = distance_to_segment(
                        px, py, size * 0.47, size * 0.50, size * 0.30, size * 0.70
                    )
                    if min(d1, d2) <= thickness / 2:
                        colour = PROMPT
                    # `_` cursor
                    if (
                        size * 0.52 <= px <= size * 0.72
                        and size * 0.615 <= py <= size * 0.70
                    ):
                        colour = CURSOR
                    r += colour[0]
                    g += colour[1]
                    b += colour[2]
                    a += 255.0
            samples = ss * ss
            if a == 0:
                row.append((0, 0, 0, 0))
            else:
                row.append(
                    (int(r / samples * (a / 255.0) / (a / 255.0) if a else 0),
                     int(g / samples),
                     int(b / samples),
                     int(a / samples))
                )
        rows.append(row)
    return rows


def png_bytes(rows):
    height = len(rows)
    width = len(rows[0])
    raw = b"".join(
        b"\x00" + bytes(v for pixel in row for v in pixel) for row in rows
    )

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico_bytes(images):
    """images: list of (size, png bytes). ICO with PNG-compressed entries."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    body = b""
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII", size if size < 256 else 0, size if size < 256 else 0, 0, 0, 1, 32,
            len(data), offset
        )
        body += data
        offset += len(data)
    return header + entries + body


def main():
    os.makedirs(ICON_DIR, exist_ok=True)
    rendered = {}
    for size in (16, 32, 48, 64, 128, 256, 512):
        rendered[size] = render(size)

    writes = {
        "32x32.png": rendered[32],
        "128x128.png": rendered[128],
        "128x128@2x.png": rendered[256],
        "icon.png": rendered[512],
    }
    for name, rows in writes.items():
        with open(os.path.join(ICON_DIR, name), "wb") as handle:
            handle.write(png_bytes(rows))

    with open(os.path.join(ICON_DIR, "icon.ico"), "wb") as handle:
        handle.write(
            ico_bytes([(size, png_bytes(rendered[size])) for size in (16, 32, 48, 64, 256)])
        )

    print("wrote icons into", os.path.normpath(ICON_DIR))
    for name in sorted(os.listdir(ICON_DIR)):
        path = os.path.join(ICON_DIR, name)
        print("  %-16s %d bytes" % (name, os.path.getsize(path)))


if __name__ == "__main__":
    main()
