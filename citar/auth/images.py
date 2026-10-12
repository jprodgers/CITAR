"""Avatar uploads, checked by their headers alone (design/phase3-features.md P3.2.6, decision 7).

An uploaded picture is the one file a member puts on the server that other people's browsers then load, so it is
an attack surface (risk 6): a "picture" that is really HTML or SVG would run script in the site's origin when opened,
and a decompression bomb would take a decoder down. Nothing here decodes anything, and no image library is added:
the file is recognised by its magic bytes as PNG, JPEG or WebP, its structure is walked far enough to know it is
whole (a cut-short upload is refused, not stored), and its dimensions are read from its header:

- **PNG**: the signature, then chunks (length, type, data, CRC) to the end, each CRC checked; IHDR first with the
  size, at least one IDAT, IEND last with nothing after it.
- **JPEG**: SOI, then marker segments to the first start-of-frame (SOF0-SOF15 but DHT, JPG and DAC), which holds
  the size; the file must end with EOI.
- **WebP**: a RIFF container whose length is the file's, form WEBP, its first chunk VP8 (lossy, the size in the key
  frame's header after its start code), VP8L (lossless, 14-bit sizes after the 0x2f signature) or VP8X (extended,
  24-bit sizes less one).

Anything else (GIF, SVG, HTML, BMP, a PNG of 2000x2000, a file over 256 KB) is refused with the rule it broke, as
``ImageError``, whose message is safe to show. The route serves the stored file with the type fixed by the format
found here, never the one the upload claimed (``ImageInfo.mime``).
"""
from __future__ import annotations

import struct
import zlib
from dataclasses import dataclass

#: The largest upload taken, in bytes.
MAX_BYTES = 256 * 1024
#: The largest width or height, in pixels.
MAX_SIDE = 1024

#: What the refusal says the rules are, so the person can act on it.
RULES = (f"An avatar is a PNG, JPEG or WebP picture of at most {MAX_BYTES // 1024} KB and {MAX_SIDE}x{MAX_SIDE} "
         "pixels.")

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
#: JPEG start-of-frame markers: C0-CF except C4 (DHT), C8 (JPG) and CC (DAC), which share the range.
_SOF = frozenset(range(0xC0, 0xD0)) - {0xC4, 0xC8, 0xCC}


class ImageError(ValueError):
    """An upload that is not an avatar this server takes; the message says why, with the rules."""


@dataclass(frozen=True)
class ImageInfo:
    """What the header said: the format, the type it is served with, the file extension, and the size."""
    format: str
    mime: str
    ext: str
    width: int
    height: int


#: format -> (served type, extension)
FORMATS = {"png": ("image/png", "png"), "jpeg": ("image/jpeg", "jpg"), "webp": ("image/webp", "webp")}
#: The extensions a stored avatar may have, and the type each is served with.
MIME_BY_EXT = {ext: mime for mime, ext in FORMATS.values()}


def _refuse(why: str) -> ImageError:
    return ImageError(f"{why} {RULES}")


def inspect(data: bytes) -> ImageInfo:
    """Recognise ``data`` as a PNG, JPEG or WebP avatar and read its size, or raise ``ImageError`` saying which rule
    it broke. Reads headers and chunk boundaries only: nothing is decompressed or decoded."""
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("inspect() takes bytes")
    data = bytes(data)
    if len(data) > MAX_BYTES:
        raise _refuse(f"That file is {len(data) // 1024} KB.")
    if not data:
        raise _refuse("That file is empty.")
    if data.startswith(_PNG_SIGNATURE):
        fmt, (width, height) = "png", _png(data)
    elif data.startswith(b"\xff\xd8\xff"):
        fmt, (width, height) = "jpeg", _jpeg(data)
    elif data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        fmt, (width, height) = "webp", _webp(data)
    else:
        raise _refuse("That is not a PNG, JPEG or WebP picture.")
    if width < 1 or height < 1:
        raise _refuse(f"That picture says it is {width}x{height} pixels.")
    if width > MAX_SIDE or height > MAX_SIDE:
        raise _refuse(f"That picture is {width}x{height} pixels.")
    mime, ext = FORMATS[fmt]
    return ImageInfo(fmt, mime, ext, width, height)


def _png(data: bytes) -> tuple:
    """A PNG's size, after walking every chunk to IEND."""
    broken = _refuse("That PNG is damaged or cut short.")
    pos, size, seen_idat, first = len(_PNG_SIGNATURE), None, False, True
    while True:
        if pos + 12 > len(data):
            raise broken
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        end = pos + 12 + length
        if length > len(data) or end > len(data):
            raise broken
        body = data[pos + 8:pos + 8 + length]
        (crc,) = struct.unpack(">I", data[end - 4:end])
        if zlib.crc32(kind + body) & 0xFFFFFFFF != crc:
            raise broken
        if first:
            if kind != b"IHDR" or length != 13:
                raise broken
            size = struct.unpack(">II", body[:8])
            first = False
        elif kind == b"IDAT":
            seen_idat = True
        elif kind == b"IEND":
            if end != len(data) or not seen_idat:
                raise broken
            return size
        pos = end


def _jpeg(data: bytes) -> tuple:
    """A JPEG's size, from its first start-of-frame segment; the file must end with EOI."""
    broken = _refuse("That JPEG is damaged or cut short.")
    if not data.endswith(b"\xff\xd9"):
        raise broken
    pos = 2
    while pos + 4 <= len(data):
        if data[pos] != 0xFF:
            raise broken
        marker = data[pos + 1]
        if marker == 0xFF:                       # fill bytes before a marker
            pos += 1
            continue
        if marker == 0xD8 or 0xD0 <= marker <= 0xD7 or marker == 0x01:   # no length: SOI, RSTn, TEM
            pos += 2
            continue
        if marker in (0xD9, 0xDA):               # EOI or start of scan before any frame header
            raise broken
        (length,) = struct.unpack(">H", data[pos + 2:pos + 4])
        if length < 2 or pos + 2 + length > len(data):
            raise broken
        if marker in _SOF:
            if length < 7:
                raise broken
            height, width = struct.unpack(">HH", data[pos + 5:pos + 9])
            return width, height
        pos += 2 + length
    raise broken


def _webp(data: bytes) -> tuple:
    """A WebP's size, from its first chunk (VP8, VP8L or VP8X); the RIFF length must be the file's."""
    broken = _refuse("That WebP is damaged or cut short.")
    (riff_size,) = struct.unpack("<I", data[4:8])
    # the RIFF length counts everything after its own field; a chunk of odd length is padded to even
    if riff_size + 8 != len(data) or len(data) < 30:
        raise broken
    kind, (length,) = data[12:16], struct.unpack("<I", data[16:20])
    payload = data[20:20 + length]
    if 20 + length > len(data) or len(payload) != length:
        raise broken
    if kind == b"VP8 ":
        # a key frame: 3 bytes of frame tag, the start code 9d 01 2a, then 14-bit width and height (2 bits of scale)
        if length < 10 or payload[3:6] != b"\x9d\x01\x2a":
            raise broken
        width, height = struct.unpack("<HH", payload[6:10])
        return width & 0x3FFF, height & 0x3FFF
    if kind == b"VP8L":
        if length < 5 or payload[0] != 0x2F:
            raise broken
        bits = int.from_bytes(payload[1:5], "little")
        return (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1
    if kind == b"VP8X":
        if length < 10:
            raise broken
        width = int.from_bytes(payload[4:7], "little") + 1
        height = int.from_bytes(payload[7:10], "little") + 1
        return width, height
    raise broken
