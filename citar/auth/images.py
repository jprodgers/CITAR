"""Avatar uploads, checked by their headers alone and stored without their metadata (design/phase3-features.md
P3.2.6, decision 7).

An uploaded picture is the one file a member puts on the server that other people's browsers then load, so it is
an attack surface (risk 6): a "picture" that is really HTML or SVG would run script in the site's origin when opened,
and a decompression bomb would take a decoder down. Nothing here decodes anything, and no image library is added:
the file is recognised by its magic bytes as PNG, JPEG or WebP, its structure is walked to its end (a cut-short or
damaged upload is refused, not stored), and its dimensions are read from its header:

- **PNG**: the signature, then chunks (length, type, data, CRC) to the end, each CRC checked and each type four
  letters; IHDR first with the size, at least one IDAT, IEND last with nothing after it, no critical chunk but
  those four and PLTE (a decoder must refuse any other).
- **JPEG**: SOI, then marker segments, each scan's entropy-coded data passed over to the marker that ends it, up to
  EOI; the first start-of-frame (SOF0-SOF15 but DHT, JPG and DAC) holds the size, and a frame and a scan must come
  before EOI.
- **WebP**: a RIFF container whose length is the file's, form WEBP, its chunks walked to the end; the first is VP8
  (lossy, the size in the key frame's header after its start code), VP8L (lossless, 14-bit sizes after the 0x2f
  signature) or VP8X (extended, 24-bit sizes less one).

Anything else (GIF, SVG, HTML, BMP, a PNG of 2000x2000, a file over 256 KB) is refused with the rule it broke, as
``ImageError``, whose message is safe to show. The route serves the stored file with the type fixed by the format
found here, never the one the upload claimed (``ImageInfo.mime``).

**Metadata is left out.** A picture goes to everyone its field reaches, so what it carries beside its pixels is
dropped before it is stored (``ImageInfo.data``): a phone's or a camera's Exif can hold where and when the photo was
taken and the camera's serial number, and a profile built around per-field privacy must not publish them. The walk
above keeps whole segments or chunks and leaves the rest out; nothing is decoded or re-encoded, so the pixels are
the upload's:

- **JPEG** keeps its frame, tables, restart interval and scans, the JFIF header (APP0, without its thumbnail, which
  can be an older crop of the photo), the ICC colour profile (APP2 ``ICC_PROFILE``) and Adobe's colour-transform
  segment (APP14), and leaves out Exif and XMP (APP1), IPTC (APP13), every other APPn, comments and anything after
  EOI (a second picture, appended bytes).
- **PNG** keeps its critical chunks and the ancillary ones that change how it looks (``_PNG_KEEP``: transparency,
  colour space, gamma, the pixel size, the background, APNG's animation), and leaves out text (tEXt, zTXt, iTXt),
  Exif (eXIf), the time (tIME) and every other chunk.
- **WebP** keeps its image, alpha, animation, ICC profile and extended header (``_WEBP_KEEP``), and leaves out EXIF,
  XMP and every other chunk; VP8X's EXIF and XMP flags are cleared and the RIFF length rewritten to match.
"""
from __future__ import annotations

import struct
import zlib
from dataclasses import dataclass, field

#: The largest upload taken, in bytes.
MAX_BYTES = 256 * 1024
#: The largest width or height, in pixels.
MAX_SIDE = 1024

#: What the refusal says the rules are, so the person can act on it.
RULES = (f"An avatar is a PNG, JPEG or WebP picture of at most {MAX_BYTES // 1024} KB and {MAX_SIDE}x{MAX_SIDE} "
         "pixels.")

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
#: The PNG critical chunks a decoder knows; a file with any other critical chunk cannot be shown.
_PNG_CRITICAL = frozenset({b"IHDR", b"PLTE", b"IDAT", b"IEND"})
#: The PNG ancillary chunks kept: those that change how the picture looks. Every other one is left out.
_PNG_KEEP = frozenset({b"tRNS", b"cHRM", b"gAMA", b"iCCP", b"sBIT", b"sRGB", b"cICP", b"mDCV", b"cLLI", b"bKGD",
                       b"pHYs", b"acTL", b"fcTL", b"fdAT"})
#: JPEG start-of-frame markers: C0-CF except C4 (DHT), C8 (JPG) and CC (DAC), which share the range.
_SOF = frozenset(range(0xC0, 0xD0)) - {0xC4, 0xC8, 0xCC}
#: JPEG markers without a length: TEM and the restart markers.
_JPEG_STANDALONE = frozenset({0x01, *range(0xD0, 0xD8)})
#: The WebP chunks kept. Every other one (EXIF, XMP, a program's own) is left out.
_WEBP_KEEP = frozenset({b"VP8X", b"VP8 ", b"VP8L", b"ALPH", b"ANIM", b"ANMF", b"ICCP"})
#: VP8X's flags saying an EXIF and an XMP chunk follow, cleared with the chunks.
_VP8X_METADATA = 0x08 | 0x04


class ImageError(ValueError):
    """An upload that is not an avatar this server takes; the message says why, with the rules."""


@dataclass(frozen=True)
class ImageInfo:
    """What the header said: the format, the type it is served with, the file extension, and the size; and the file
    as it is stored and served (``data``: the upload without its metadata)."""
    format: str
    mime: str
    ext: str
    width: int
    height: int
    data: bytes = field(default=b"", repr=False, compare=False)


#: format -> (served type, extension)
FORMATS = {"png": ("image/png", "png"), "jpeg": ("image/jpeg", "jpg"), "webp": ("image/webp", "webp")}
#: The extensions a stored avatar may have, and the type each is served with.
MIME_BY_EXT = {ext: mime for mime, ext in FORMATS.values()}


def _refuse(why: str) -> ImageError:
    return ImageError(f"{why} {RULES}")


def inspect(data: bytes) -> ImageInfo:
    """Recognise ``data`` as a PNG, JPEG or WebP avatar, read its size and leave its metadata out (``ImageInfo.data``),
    or raise ``ImageError`` saying which rule it broke. Reads headers and segment or chunk boundaries only: nothing is
    decompressed or decoded."""
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("inspect() takes bytes")
    data = bytes(data)
    if len(data) > MAX_BYTES:
        raise _refuse(f"That file is {len(data) // 1024} KB.")
    if not data:
        raise _refuse("That file is empty.")
    if data.startswith(_PNG_SIGNATURE):
        fmt, walk = "png", _png
    elif data.startswith(b"\xff\xd8\xff"):
        fmt, walk = "jpeg", _jpeg
    elif data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        fmt, walk = "webp", _webp
    else:
        raise _refuse("That is not a PNG, JPEG or WebP picture.")
    (width, height), kept = walk(data)
    if width < 1 or height < 1:
        raise _refuse(f"That picture says it is {width}x{height} pixels.")
    if width > MAX_SIDE or height > MAX_SIDE:
        raise _refuse(f"That picture is {width}x{height} pixels.")
    mime, ext = FORMATS[fmt]
    return ImageInfo(fmt, mime, ext, width, height, kept)


def _png(data: bytes) -> tuple:
    """A PNG's size and the file without its metadata chunks, after walking every chunk to IEND."""
    broken = _refuse("That PNG is damaged or cut short.")
    pos, size, seen_idat, first = len(_PNG_SIGNATURE), None, False, True
    kept = [_PNG_SIGNATURE]
    while True:
        if pos + 12 > len(data):
            raise broken
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        end = pos + 12 + length
        if length > len(data) or end > len(data) or not kind.isalpha():      # bytes.isalpha: ASCII letters
            raise broken
        body = data[pos + 8:pos + 8 + length]
        (crc,) = struct.unpack(">I", data[end - 4:end])
        if zlib.crc32(kind + body) & 0xFFFFFFFF != crc:
            raise broken
        critical = not kind[0] & 0x20
        if critical and kind not in _PNG_CRITICAL:
            raise broken
        if critical or kind in _PNG_KEEP:
            kept.append(data[pos:end])
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
            return size, b"".join(kept)
        pos = end


def _jpeg_kept(marker: int, segment: bytes) -> bytes:
    """A JPEG marker segment as it is stored: itself, nothing (metadata), or the JFIF header without its
    thumbnail."""
    body = segment[4:]
    if marker == 0xFE:                                   # COM: a comment
        return b""
    if 0xE0 <= marker <= 0xEF:                           # APPn
        if marker == 0xE0 and body.startswith(b"JFIF\0") and len(body) >= 14:
            # version, units and density (7 bytes after the identifier), then a thumbnail of 0x0
            return b"\xff\xe0\x00\x10" + body[:12] + b"\x00\x00"
        if marker == 0xE2 and body.startswith(b"ICC_PROFILE\0"):
            return segment
        if marker == 0xEE and body.startswith(b"Adobe"):
            return segment
        return b""
    return segment


def _scan_end(data: bytes, pos: int, broken: ImageError) -> int:
    """Where a scan's entropy-coded data, starting at ``pos``, ends: the first 0xFF that is not a stuffed zero or a
    restart marker."""
    while True:
        at = data.find(b"\xff", pos)
        if at < 0 or at + 1 >= len(data):
            raise broken
        following = data[at + 1]
        if following == 0x00 or 0xD0 <= following <= 0xD7:
            pos = at + 2
            continue
        return at


def _jpeg(data: bytes) -> tuple:
    """A JPEG's size, from its first start-of-frame segment, and the file without its metadata, after walking every
    segment and scan to EOI."""
    broken = _refuse("That JPEG is damaged or cut short.")
    pos, size, seen_scan = 2, None, False
    kept = [b"\xff\xd8"]
    while True:
        if pos + 2 > len(data) or data[pos] != 0xFF:
            raise broken
        marker = data[pos + 1]
        if marker == 0xFF:                               # fill bytes before a marker
            pos += 1
            continue
        if marker == 0xD9:                               # EOI: the picture ends; anything after it is left out
            if size is None or not seen_scan:
                raise broken
            kept.append(b"\xff\xd9")
            return size, b"".join(kept)
        if marker in _JPEG_STANDALONE:
            kept.append(data[pos:pos + 2])
            pos += 2
            continue
        if marker == 0xD8 or pos + 4 > len(data):        # a second SOI, or no room for a length
            raise broken
        (length,) = struct.unpack(">H", data[pos + 2:pos + 4])
        end = pos + 2 + length
        if length < 2 or end > len(data):
            raise broken
        if marker in _SOF and size is None:
            if length < 7:
                raise broken
            height, width = struct.unpack(">HH", data[pos + 5:pos + 9])
            size = (width, height)
        kept.append(_jpeg_kept(marker, data[pos:end]))
        pos = end
        if marker == 0xDA:                               # SOS: the scan's data follows its header
            if size is None:
                raise broken
            seen_scan = True
            scan_end = _scan_end(data, pos, broken)
            kept.append(data[pos:scan_end])
            pos = scan_end


def _webp_size(kind: bytes, payload: bytes, broken: ImageError) -> tuple:
    """A WebP's size, from its first chunk (VP8, VP8L or VP8X)."""
    if kind == b"VP8 ":
        # a key frame: 3 bytes of frame tag, the start code 9d 01 2a, then 14-bit width and height (2 bits of scale)
        if len(payload) < 10 or payload[3:6] != b"\x9d\x01\x2a":
            raise broken
        width, height = struct.unpack("<HH", payload[6:10])
        return width & 0x3FFF, height & 0x3FFF
    if kind == b"VP8L":
        if len(payload) < 5 or payload[0] != 0x2F:
            raise broken
        bits = int.from_bytes(payload[1:5], "little")
        return (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1
    if kind == b"VP8X":
        if len(payload) < 10:
            raise broken
        return int.from_bytes(payload[4:7], "little") + 1, int.from_bytes(payload[7:10], "little") + 1
    raise broken


def _webp(data: bytes) -> tuple:
    """A WebP's size, from its first chunk, and the file without its metadata chunks, after walking every chunk to
    the end; the RIFF length must be the file's."""
    broken = _refuse("That WebP is damaged or cut short.")
    (riff_size,) = struct.unpack("<I", data[4:8])
    # the RIFF length counts everything after its own field
    if riff_size + 8 != len(data) or len(data) < 30:
        raise broken
    pos, size, kept = 12, None, []
    while pos < len(data):
        if pos + 8 > len(data):
            raise broken
        kind = data[pos:pos + 4]
        (length,) = struct.unpack("<I", data[pos + 4:pos + 8])
        end = pos + 8 + length
        if end > len(data):
            raise broken
        chunk = data[pos:end]
        if size is None:
            size = _webp_size(kind, chunk[8:], broken)
            if kind == b"VP8X":
                chunk = chunk[:8] + bytes([chunk[8] & ~_VP8X_METADATA & 0xFF]) + chunk[9:]
        elif kind == b"VP8X":                            # the extended header comes first or not at all
            raise broken
        if kind in _WEBP_KEEP:
            # a chunk of odd length is padded to even; the pad is written afresh (a zero, as RIFF says)
            kept.append(chunk + (b"\0" if length & 1 else b""))
        # the last chunk's pad may be missing from a file whose RIFF length counts without it
        pos = min(end + (length & 1), len(data))
    body = b"WEBP" + b"".join(kept)
    return size, b"RIFF" + struct.pack("<I", len(body)) + body
