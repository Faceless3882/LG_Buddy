#!/usr/bin/env python3

from __future__ import annotations

import struct
import sys
from pathlib import Path


XWD_HEADER_WORDS = 25
XWD_HEADER_BYTES = XWD_HEADER_WORDS * 4
XWD_FILE_VERSION = 7
Z_PIXMAP = 2
LSB_FIRST = 0


def channel(pixel: int, mask: int) -> float:
    if mask == 0:
        return 0.0
    shift = (mask & -mask).bit_length() - 1
    maximum = mask >> shift
    return ((pixel & mask) >> shift) / maximum


def mean_luminance(path: Path) -> float:
    content = path.read_bytes()
    if len(content) < XWD_HEADER_BYTES:
        raise ValueError("XWD header is truncated")
    header = struct.unpack_from(">25I", content)
    (
        header_size,
        file_version,
        pixmap_format,
        _pixmap_depth,
        width,
        height,
        _xoffset,
        byte_order,
        _bitmap_unit,
        _bitmap_bit_order,
        _bitmap_pad,
        bits_per_pixel,
        bytes_per_line,
        _visual_class,
        red_mask,
        green_mask,
        blue_mask,
        _bits_per_rgb,
        _colormap_entries,
        color_count,
        *_window_geometry,
    ) = header
    if file_version != XWD_FILE_VERSION or pixmap_format != Z_PIXMAP:
        raise ValueError("unsupported XWD format")
    if bits_per_pixel not in (24, 32) or width == 0 or height == 0:
        raise ValueError("unsupported XWD pixel layout")

    pixel_bytes = bits_per_pixel // 8
    data_offset = header_size + color_count * 12
    data_size = bytes_per_line * height
    if data_offset > len(content) or data_size > len(content) - data_offset:
        raise ValueError("XWD pixel data is truncated")
    byte_order_name = "little" if byte_order == LSB_FIRST else "big"

    luminance = 0.0
    samples = 0
    for y in range(height):
        row = data_offset + y * bytes_per_line
        for x in range(width):
            start = row + x * pixel_bytes
            pixel = int.from_bytes(
                content[start : start + pixel_bytes], byte_order_name
            )
            red = channel(pixel, red_mask)
            green = channel(pixel, green_mask)
            blue = channel(pixel, blue_mask)
            luminance += 0.2126 * red + 0.7152 * green + 0.0722 * blue
            samples += 1
    return luminance / samples


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} <screenshot.xwd>")
    try:
        print(f"{mean_luminance(Path(sys.argv[1])):.6f}")
    except (OSError, ValueError) as error:
        raise SystemExit(f"cannot inspect XWD screenshot: {error}") from error
    return 0


if __name__ == "__main__":
    sys.exit(main())
