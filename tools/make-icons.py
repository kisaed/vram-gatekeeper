"""Generates the application icons (no third-party imaging library needed).

Draws a dark rounded tile with a gauge ring (VRAM) and a gate bar across it,
supersampled for smooth edges, then writes the PNG sizes Tauri expects and a
multi-size Windows .ico (PNG-compressed entries, supported since Vista).

Run:  py tools/make-icons.py [output-dir]
Default output: app/src-tauri/icons
"""
import math
import os
import struct
import sys
import zlib

SS = 3  # supersampling factor per axis


def png_bytes(width, height, rgba):
    """Encodes an RGBA buffer (bytes) as a PNG file."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")


def render(size):
    """Returns RGBA bytes for the icon at `size` px."""
    n = size * SS
    cx = cy = n / 2.0
    out = bytearray(size * size * 4)
    bg = (24, 31, 45)
    ring = (45, 212, 191)    # teal: free VRAM
    used = (96, 165, 250)    # blue: used VRAM
    gate = (248, 250, 252)   # near white

    for py in range(size):
        for px in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(SS):
                for sx in range(SS):
                    x = px * SS + sx + 0.5
                    y = py * SS + sy + 0.5
                    r, g, b, a = pixel(x, y, n, cx, cy, bg, ring, used, gate)
                    acc[0] += r * a
                    acc[1] += g * a
                    acc[2] += b * a
                    acc[3] += a
            total = SS * SS
            o = (py * size + px) * 4
            if acc[3] > 0:
                out[o] = int(acc[0] / acc[3])
                out[o + 1] = int(acc[1] / acc[3])
                out[o + 2] = int(acc[2] / acc[3])
            out[o + 3] = int(255 * acc[3] / total)
    return bytes(out)


def pixel(x, y, n, cx, cy, bg, ring, used, gate):
    """Colour and coverage (0 or 1) of one supersample."""
    # Rounded tile.
    radius = n * 0.22
    margin = n * 0.04
    dx = max(abs(x - cx) - (n / 2 - margin - radius), 0)
    dy = max(abs(y - cy) - (n / 2 - margin - radius), 0)
    if dx * dx + dy * dy > radius * radius:
        return (0, 0, 0, 0.0)

    colour = bg
    dist = math.hypot(x - cx, y - cy)
    r_outer, r_inner = n * 0.34, n * 0.24
    if r_inner <= dist <= r_outer:
        # Gauge: 270 degree arc starting bottom-left, clockwise; 65% "used".
        ang = (math.degrees(math.atan2(y - cy, x - cx)) - 135.0) % 360.0
        if ang <= 270.0:
            colour = used if ang <= 270.0 * 0.65 else ring
    # The gate bar across the middle.
    if abs(y - cy) <= n * 0.045 and abs(x - cx) <= n * 0.30:
        colour = gate
    return (colour[0], colour[1], colour[2], 1.0)


def ico_bytes(images):
    """images: list of (size, png_bytes). Returns a .ico file."""
    count = len(images)
    header = struct.pack("<HHH", 0, 1, count)
    entries = b""
    offset = 6 + 16 * count
    blobs = b""
    for size, png in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset)
        blobs += png
        offset += len(png)
    return header + entries + blobs


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, "app", "src-tauri", "icons")
    os.makedirs(out_dir, exist_ok=True)
    pngs = {s: png_bytes(s, s, render(s)) for s in (16, 32, 48, 64, 128, 256)}
    big = png_bytes(512, 512, render(512))
    files = {
        "32x32.png": pngs[32],
        "128x128.png": pngs[128],
        "128x128@2x.png": pngs[256],
        "icon.png": big,
        "icon.ico": ico_bytes([(s, pngs[s]) for s in (16, 32, 48, 64, 128, 256)]),
    }
    for name, data in files.items():
        with open(os.path.join(out_dir, name), "wb") as f:
            f.write(data)
        print(f"wrote {name} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
