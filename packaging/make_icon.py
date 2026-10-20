"""Generates packaging/icons/app.ico and app.png (the window icon of src/gui/theme.rs).

The drawing mirrors `theme::icon_rgba`: a monitor glyph on an accent-blue disc.
Run with:  python packaging/make_icon.py
"""
import os
import struct
import zlib

ACCENT = (0x00, 0xAA, 0xDE)
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "icons")


def icon_rgba(size, color):
    """Returns size*size RGBA bytes; same geometry as theme::icon_rgba."""
    s = float(size)
    px = bytearray(size * size * 4)
    cx = cy = s / 2.0
    r = s / 2.0 - 0.5
    sx0, sx1, sy0, sy1 = s * 0.24, s * 0.76, s * 0.27, s * 0.62
    frame = max(s * 0.07, 1.0)
    for y in range(size):
        for x in range(size):
            fx, fy = x + 0.5, y + 0.5
            d = ((fx - cx) ** 2 + (fy - cy) ** 2) ** 0.5
            cover = min(max(r - d + 0.5, 0.0), 1.0)
            if cover <= 0.0:
                continue
            in_screen = sx0 <= fx <= sx1 and sy0 <= fy <= sy1
            in_frame = in_screen and (fx < sx0 + frame or fx > sx1 - frame or fy < sy0 + frame or fy > sy1 - frame)
            in_stand = (abs(fx - cx) <= frame * 0.75 and sy1 < fy <= sy1 + s * 0.08) or (
                abs(fx - cx) <= s * 0.14 and sy1 + s * 0.08 < fy <= sy1 + s * 0.08 + frame
            )
            c = (255, 255, 255) if in_frame or in_stand else color
            i = (y * size + x) * 4
            px[i:i + 4] = bytes((c[0], c[1], c[2], int(cover * 255)))
    return bytes(px)


def png(size, rgba):
    """Encodes RGBA pixels as a PNG file."""
    raw = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    os.makedirs(OUT, exist_ok=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [png(n, icon_rgba(n, ACCENT)) for n in sizes]
    # ICO container with PNG-compressed entries (supported since Windows Vista).
    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries, blobs = b"", b""
    for n, img in zip(sizes, images):
        dim = 0 if n >= 256 else n
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(img), offset)
        blobs += img
        offset += len(img)
    with open(os.path.join(OUT, "app.ico"), "wb") as f:
        f.write(header + entries + blobs)
    with open(os.path.join(OUT, "app.png"), "wb") as f:
        f.write(images[-1])
    print("wrote", OUT)


if __name__ == "__main__":
    main()
