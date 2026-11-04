"""FDS format 4: a second implementation, written from SPEC.md §6 alone.

Python standard library only. It reads and writes `.fds` files, and it exists
for one reason: if two programs written separately from the same text agree on
every file in `cases/`, the text is enough to implement the format. It does
not share a line with the Rust crate.

    import fds
    meta, records = fds.read("file.fds")
    open("copy.fds", "wb").write(fds.encode(meta, records))

Errors are `FdsError` with a `.category` that is one of the five names in
SPEC.md §6.6: not_fds, unsupported_major, truncated, checksum, malformed.
"""

import struct
import zlib

MAGIC = b"\x89FDS\r\n\x1a\n"
MAJOR = 4
MINOR = 0
HEADER_LEN = 64
INDEX_ENTRY_LEN = 16
MAX_DEPTH = 32

KINDS = ("fact", "procedure", "knowledge")


class FdsError(Exception):
    def __init__(self, category, detail=""):
        super().__init__(f"{category}: {detail}" if detail else category)
        self.category = category
        self.detail = detail


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


# ── CBOR (RFC 8949): the subset FDS uses ─────────────────────────────────


def _head(major, n):
    m = major << 5
    if n < 24:
        return bytes([m | n])
    if n <= 0xFF:
        return bytes([m | 24, n])
    if n <= 0xFFFF:
        return bytes([m | 25]) + struct.pack(">H", n)
    if n <= 0xFFFFFFFF:
        return bytes([m | 26]) + struct.pack(">I", n)
    return bytes([m | 27]) + struct.pack(">Q", n)


def _text(s):
    b = s.encode("utf-8")
    return _head(3, len(b)) + b


def _bytes(b):
    return _head(2, len(b)) + b


def _uint(n):
    return _head(0, n)


def _map(entries):
    """entries: list of (encoded key, encoded value). Keys sorted by their
    encoded bytes, which is RFC 8949 §4.2.1 for text keys."""
    entries = sorted(entries, key=lambda kv: kv[0])
    return _head(5, len(entries)) + b"".join(k + v for k, v in entries)


def _array(items):
    return _head(4, len(items)) + b"".join(items)


def _malformed(why):
    return FdsError("malformed", why)


class _Reader:
    def __init__(self, buf):
        self.buf = buf
        self.pos = 0

    def done(self):
        return self.pos == len(self.buf)

    def take(self, n):
        if n > len(self.buf) - self.pos:
            raise _malformed("an item runs past the end of its data")
        out = self.buf[self.pos : self.pos + n]
        self.pos += n
        return out

    def head(self):
        b = self.take(1)[0]
        major, info = b >> 5, b & 0x1F
        if info < 24:
            return major, info
        if info == 24:
            return major, self.take(1)[0]
        if info == 25:
            return major, struct.unpack(">H", self.take(2))[0]
        if info == 26:
            return major, struct.unpack(">I", self.take(4))[0]
        if info == 27:
            return major, struct.unpack(">Q", self.take(8))[0]
        if info == 31:
            raise _malformed("indefinite-length items are not part of FDS")
        raise _malformed("reserved additional information")

    def uint(self):
        major, n = self.head()
        if major != 0:
            raise _malformed("expected an unsigned integer")
        return n

    def bytes_(self):
        major, n = self.head()
        if major != 2:
            raise _malformed("expected a byte string")
        return self.take(n)

    def text(self):
        major, n = self.head()
        if major != 3:
            raise _malformed("expected text")
        raw = self.take(n)
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError:
            raise _malformed("text is not valid UTF-8") from None

    def text_or_null(self):
        if self.pos < len(self.buf) and self.buf[self.pos] == 0xF6:
            self.pos += 1
            return None
        return self.text()

    def map_len(self):
        major, n = self.head()
        if major != 5:
            raise _malformed("expected a map")
        if n * 2 > len(self.buf) - self.pos:
            raise _malformed("map longer than its data")
        return n

    def array_len(self):
        major, n = self.head()
        if major != 4:
            raise _malformed("expected an array")
        if n > len(self.buf) - self.pos:
            raise _malformed("array longer than its data")
        return n

    def skip(self, depth=0):
        if depth > MAX_DEPTH:
            raise _malformed("nesting too deep")
        major, n = self.head()
        if major in (0, 1, 7):
            return
        if major in (2, 3):
            self.take(n)
        elif major == 4:
            if n > len(self.buf) - self.pos:
                raise _malformed("array longer than its data")
            for _ in range(n):
                self.skip(depth + 1)
        elif major == 5:
            if n * 2 > len(self.buf) - self.pos:
                raise _malformed("map longer than its data")
            for _ in range(2 * n):
                self.skip(depth + 1)
        else:  # 6: a tag wraps one item
            self.skip(depth + 1)


# ── records and metadata (SPEC.md §6.4) ──────────────────────────────────


def encode_record(r):
    e = [
        (_text("id"), _text(r["id"])),
        (_text("kind"), _text(r["kind"])),
        (_text("collection"), _text(r["collection"])),
        (_text("body"), _text(r["body"])),
        (_text("title"), _text(r["title"])),
        (_text("source"), _text(r["source"])),
        (_text("verified_by"), _text(r["verified_by"])),
        (_text("last_verified"), _text(r["last_verified"])),
        (_text("tokens"), _uint(r["tokens"])),
    ]
    if r.get("question") is not None:
        e.append((_text("question"), _text(r["question"])))
    if r.get("embedding") is not None:
        e.append((_text("embedding"), _bytes(b"".join(struct.pack("<f", x) for x in r["embedding"]))))
    if r.get("type"):
        e.append((_text("type"), _text(r["type"])))
    if r.get("attrs"):
        e.append((_text("attrs"), _map([(_text(k), _text(v)) for k, v in r["attrs"].items()])))
    if r.get("links"):
        e.append((_text("links"), _array([_text(x) for x in r["links"]])))
    return _map(e)


def encode_meta(m):
    return _map(
        [
            (_text("name"), _text(m["name"])),
            (_text("created"), _text(m["created"])),
            (_text("updated"), _text(m["updated"])),
            (_text("embedding_model"), _text(m["embedding_model"])),
            (_text("embedding_dims"), _uint(m["embedding_dims"])),
        ]
    )


def _u32(n, what):
    if n > 0xFFFFFFFF:
        raise _malformed(f"{what} does not fit in 32 bits")
    return n


def decode_record(buf):
    r = _Reader(buf)
    seen = {}

    def put(key, value):
        if key in seen:
            raise _malformed(f"key `{key}` appears twice")
        seen[key] = value

    for _ in range(r.map_len()):
        key = r.text()
        if key in ("id", "kind", "collection", "body", "title", "source", "verified_by", "last_verified", "type"):
            put(key, r.text())
        elif key == "question":
            put(key, r.text_or_null())
        elif key == "tokens":
            put(key, _u32(r.uint(), "tokens"))
        elif key == "embedding":
            raw = r.bytes_()
            if len(raw) % 4:
                raise _malformed("embedding is not a whole number of f32")
            put(key, [struct.unpack("<f", raw[i : i + 4])[0] for i in range(0, len(raw), 4)])
        elif key == "attrs":
            attrs = {}
            for _ in range(r.map_len()):
                k, v = r.text(), r.text()
                if k in attrs:
                    raise _malformed("an attribute appears twice")
                attrs[k] = v
            put(key, attrs)
        elif key == "links":
            put(key, [r.text() for _ in range(r.array_len())])
        else:
            r.skip()
    if not r.done():
        raise _malformed("bytes after the end of a record")
    for required in ("id", "kind", "collection", "body", "title", "source", "verified_by", "last_verified", "tokens"):
        if required not in seen:
            raise _malformed(f"required key `{required}` is missing")
    if seen["kind"] not in KINDS:
        raise _malformed(f"unknown kind `{seen['kind']}`")
    return {
        "id": seen["id"],
        "kind": seen["kind"],
        "collection": seen["collection"],
        "question": seen.get("question"),
        "body": seen["body"],
        "title": seen["title"],
        "source": seen["source"],
        "verified_by": seen["verified_by"],
        "last_verified": seen["last_verified"],
        "tokens": seen["tokens"],
        "embedding": seen.get("embedding"),
        "type": seen.get("type", ""),
        "attrs": seen.get("attrs", {}),
        "links": seen.get("links", []),
    }


def decode_meta(buf):
    r = _Reader(buf)
    m = {"name": "", "created": "", "updated": "", "embedding_model": "", "embedding_dims": 0}
    for _ in range(r.map_len()):
        key = r.text()
        if key in ("name", "created", "updated", "embedding_model"):
            m[key] = r.text()
        elif key == "embedding_dims":
            m[key] = _u32(r.uint(), "embedding_dims")
        else:
            r.skip()
    if not r.done():
        raise _malformed("bytes after the end of the metadata")
    return m


# ── the file (SPEC.md §6.2 to §6.6) ──────────────────────────────────────


def encode(meta, records, application_id=0, user_version=0):
    meta_blob = encode_meta(meta)
    blobs = [encode_record(r) for r in records]
    records_start = HEADER_LEN + len(meta_blob)
    index_offset = records_start + sum(len(b) for b in blobs)
    index = b""
    cursor = records_start
    for b in blobs:
        index += struct.pack("<QII", cursor, len(b), crc32(b))
        cursor += len(b)
    header = (
        MAGIC
        + struct.pack("<HH", MAJOR, MINOR)
        + struct.pack("<II", application_id, user_version)
        + struct.pack("<I", len(records))
        + struct.pack("<Q", HEADER_LEN)
        + struct.pack("<I", len(meta_blob))
        + struct.pack("<Q", index_offset)
        + struct.pack("<II", crc32(meta_blob), crc32(index))
        + bytes(8)
    )
    header += struct.pack("<I", crc32(header))
    assert len(header) == HEADER_LEN
    return header + meta_blob + b"".join(blobs) + index


class Database:
    """An opened file. `get(i)` reads one record and checks only its CRC."""

    def __init__(self, data):
        # The five checks of SPEC.md §6.6, in order.
        n = min(len(data), len(MAGIC))
        if data[:n] != MAGIC[:n]:
            raise FdsError("not_fds")
        if len(data) < 10:
            raise FdsError("truncated", "the file ends inside the magic or the version")
        major = struct.unpack_from("<H", data, 8)[0]
        if major != MAJOR:
            raise FdsError("unsupported_major", str(major))
        if len(data) < HEADER_LEN:
            raise FdsError("truncated", "the file ends inside the header")
        if crc32(data[:60]) != struct.unpack_from("<I", data, 60)[0]:
            raise FdsError("checksum", "header")

        self.data = data
        self.minor = struct.unpack_from("<H", data, 10)[0]
        self.application_id, self.user_version, self.count = struct.unpack_from("<III", data, 12)
        meta_offset, meta_length, index_offset, meta_crc, index_crc = struct.unpack_from("<QIQII", data, 24)

        meta_end = meta_offset + meta_length
        if meta_end > len(data):
            raise FdsError("truncated", "the metadata runs past the end of the file")
        if meta_offset < HEADER_LEN:
            raise _malformed("the metadata overlaps the header")
        meta_bytes = data[meta_offset:meta_end]
        if crc32(meta_bytes) != meta_crc:
            raise FdsError("checksum", "metadata")

        index_end = index_offset + self.count * INDEX_ENTRY_LEN
        if index_end > len(data):
            raise FdsError("truncated", "the index runs past the end of the file")
        if index_offset < meta_end:
            raise _malformed("the index overlaps the metadata")
        if crc32(data[index_offset:index_end]) != index_crc:
            raise FdsError("checksum", "index")

        self.records_start = meta_end
        self.index_offset = index_offset
        self.meta = decode_meta(meta_bytes)

    def get(self, i):
        if not 0 <= i < self.count:
            raise IndexError(i)
        offset, length, crc = struct.unpack_from("<QII", self.data, self.index_offset + i * INDEX_ENTRY_LEN)
        end = offset + length
        if offset < self.records_start or end > self.index_offset:
            raise _malformed("a record lies outside the records region")
        blob = self.data[offset:end]
        if crc32(blob) != crc:
            raise FdsError("checksum", "record")
        return decode_record(blob)

    def records(self):
        return [self.get(i) for i in range(self.count)]


def read(path):
    with open(path, "rb") as f:
        db = Database(f.read())
    return db.meta, db.records()
