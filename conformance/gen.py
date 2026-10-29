"""Builds every file in cases/ and its expected result.

The golden files are made by `fds.py`, the Python implementation, and by hand
for the damaged ones: nothing here calls the Rust crate. Run it again and the
output must not change (the files are deterministic); a diff in `git status`
after running it means something changed in the format.

    python conformance/gen.py
"""

import json
import os
import shutil
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fds  # noqa: E402
from fds import _array, _bytes, _head, _map, _text, _uint, crc32  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
CASES = os.path.join(HERE, "cases")


def rec(n, **over):
    r = {
        "id": f"{n:016x}",
        "kind": "fact",
        "collection": "default",
        "question": f"question {n}",
        "body": f"answer {n}",
        "title": f"question {n}",
        "source": "conformance",
        "verified_by": "human",
        "last_verified": "2026-10-19",
        "tokens": 2,
        "embedding": None,
        "type": "",
        "attrs": {},
        "links": [],
    }
    r.update(over)
    return r


META = {"name": "conformance", "created": "2026-10-19", "updated": "2026-10-19", "embedding_model": "", "embedding_dims": 0}


def seal(data, *, header=True, index=True):
    """Recomputes the CRCs of a file whose fields were edited by hand."""
    b = bytearray(data)
    if index:
        count = struct.unpack_from("<I", b, 20)[0]
        index_offset = struct.unpack_from("<Q", b, 36)[0]
        struct.pack_into("<I", b, 48, crc32(bytes(b[index_offset : index_offset + count * 16])))
    if header:
        struct.pack_into("<I", b, 60, crc32(bytes(b[:60])))
    return bytes(b)


def assemble(meta_blob, blobs, *, minor=0, reserved=bytes(8), application_id=0, user_version=0):
    """A file built from raw record bytes, with every CRC correct."""
    records_start = fds.HEADER_LEN + len(meta_blob)
    index_offset = records_start + sum(len(b) for b in blobs)
    index = b""
    cursor = records_start
    for b in blobs:
        index += struct.pack("<QII", cursor, len(b), crc32(b))
        cursor += len(b)
    h = (
        fds.MAGIC
        + struct.pack("<HHIII", fds.MAJOR, minor, application_id, user_version, len(blobs))
        + struct.pack("<QIQII", fds.HEADER_LEN, len(meta_blob), index_offset, crc32(meta_blob), crc32(index))
        + reserved
    )
    h += struct.pack("<I", crc32(h))
    return h + meta_blob + b"".join(blobs) + index


def kv(key, encoded_value):
    return (_text(key), encoded_value)


def record_entries(r):
    """A good record as a list of (key, value) pairs, for editing by hand."""
    return [
        kv("id", _text(r["id"])),
        kv("kind", _text(r["kind"])),
        kv("collection", _text(r["collection"])),
        kv("body", _text(r["body"])),
        kv("title", _text(r["title"])),
        kv("source", _text(r["source"])),
        kv("verified_by", _text(r["verified_by"])),
        kv("last_verified", _text(r["last_verified"])),
        kv("tokens", _uint(r["tokens"])),
    ]


def write_case(group, name, data, expected, description):
    d = os.path.join(CASES, group)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name + ".fds"), "wb") as f:
        f.write(data)
    expected = {"description": description, **expected}
    with open(os.path.join(d, name + ".json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(expected, f, ensure_ascii=False, indent=2)
        f.write("\n")


def valid(name, meta, records, description, *, data=None, minor=0, writable=True, reproducible=True):
    data = data if data is not None else fds.encode(meta, records)
    write_case(
        "valid",
        name,
        data,
        {"minor": minor, "writable": writable, "reproducible": reproducible, "meta": meta, "records": records},
        description,
    )


def invalid(name, data, error, description, *, stage="open", readable=None):
    exp = {"error": error, "stage": stage}
    if readable is not None:
        exp["readable"] = readable
    write_case("invalid", name, data, exp, description)


def main():
    if os.path.isdir(CASES):
        shutil.rmtree(CASES)

    # ── valid ────────────────────────────────────────────────────────────
    valid("empty", {**META, "name": "empty"}, [], "No records: the smallest legal file.")
    valid("one_fact", META, [rec(1)], "One fact with every optional field absent.")

    knowledge = rec(
        3,
        kind="knowledge",
        question=None,
        title="Rear panel of the router",
        body="Four yellow LAN ports, one blue WAN. See [[reset the router]].",
        embedding=[0.25, -0.5, 1.0, 0.0],
        type="image",
        attrs={"file": "photos/router.jpg", "described_by": "moondream", "x": "1", "yy": "2"},
        links=["reset the router", "router admin address"],
        tokens=12,
    )
    procedure = rec(
        2,
        kind="procedure",
        question="flush the dns cache",
        body='{"steps":[{"action":"run","command":"ipconfig /flushdns"}]}',
        type="command",
        attrs={"shell": "powershell"},
    )
    valid(
        "all_fields",
        {**META, "embedding_model": "all-minilm", "embedding_dims": 4},
        [rec(1), procedure, knowledge, rec(4, question="", embedding=[])],
        "All three kinds; every optional field present at least once. Some(\"\") and an empty "
        "embedding are different from absent, and must survive.",
    )
    valid(
        "attrs_unsorted",
        META,
        [rec(1, attrs={"b": "2", "a": "1", "aa": "3", "é": "4", "z" * 30: "5"}, links=["z", "a"])],
        "Attribute keys of different lengths: the file orders them by their encoding (shorter first), "
        "and `links` keeps the order it was written in.",
    )
    valid(
        "unicode",
        {**META, "name": "ñandú — 日本語"},
        [
            rec(
                1,
                question="¿Cuál es la tensión máxima?",
                body="Línea 1\nLínea 2\r\n\ttabulada, con NUL \u0000 y emoji 😀 y 𝔘𝔫𝔦𝔠𝔬𝔡𝔢",
                title="日本語のテキスト",
            )
        ],
        "Text is UTF-8, including a newline, a NUL, an emoji and a character outside the BMP.",
    )
    lengths = [0, 23, 24, 255, 256, 65535, 65536]
    boundary = [
        rec(i + 1, body="x" * n, title="t" * min(n, 300), tokens=t)
        for i, (n, t) in enumerate(zip(lengths, [0, 23, 24, 255, 256, 65535, 4294967295]))
    ]
    valid(
        "head_boundaries",
        META,
        boundary,
        "Text lengths and integers on both sides of every CBOR head width (23/24, 255/256, 65535/65536); "
        "a writer must use the shortest head, or the bytes differ.",
    )
    valid("many_records", META, [rec(i) for i in range(257)], "257 records: the count does not fit in a byte.")

    # A file from the future: minor 3, reserved bytes in use, a nonzero application_id,
    # and keys this version has never heard of, of every CBOR type.
    future_meta = _map(
        [kv("name", _text("from the future")), kv("created", _text("2030-01-01")), kv("updated", _text("2030-01-01")),
         kv("embedding_model", _text("")), kv("embedding_dims", _uint(0)), kv("license", _text("CC0"))]
    )
    unknown_values = _map(
        [
            kv("float", bytes([0xF9, 0x3E, 0x00])),  # 1.5, half precision
            kv("negative", bytes([0x38, 0x63])),  # -100
            kv("flag", bytes([0xF5])),  # true
            kv("tagged", bytes([0xC1, 0x1A, 0x65, 0x00, 0x00, 0x00])),  # tag 1 (epoch time)
            kv("nested", _array([_uint(1), bytes([0xF6]), _map([kv("deeper", _array([_text("x")]))])])),
            kv("blob", _bytes(b"\x00\x01\x02")),
        ]
    )
    future_blobs = []
    for n in (1, 2):
        entries = record_entries(rec(n, question=None)) + [kv("sparkle", unknown_values), kv("rating", _uint(5))]
        future_blobs.append(_map(entries))
    future_data = assemble(future_meta, future_blobs, minor=3, reserved=b"\xde\xad\xbe\xef\x00\x01\x02\x03",
                           application_id=7, user_version=9)
    valid(
        "future_minor",
        {**META, "name": "from the future", "created": "2030-01-01", "updated": "2030-01-01"},
        [rec(1, question=None), rec(2, question=None)],
        "Written by format 4.3: unknown keys (every CBOR type) in the metadata and in the records, non-zero "
        "reserved bytes, application_id 7, user_version 9. A 4.0 reader reads it, ignoring what it does not "
        "know, and a 4.0 writer refuses to rewrite it.",
        data=future_data, minor=3, writable=False, reproducible=False,
    )

    # What a reader must accept that a writer must not produce (R1): keys in any
    # order, integers and lengths in a longer form than necessary, null for "no question".
    lenient = record_entries(rec(1, question=None))[::-1]
    lenient = [(k, bytes([0x18, 0x02]) if k == _text("tokens") else bytes([0x78, 16]) + b"0000000000000001" if k == _text("id") else v)
               for k, v in lenient] + [kv("question", bytes([0xF6]))]
    unsorted_map = lambda entries: _head(5, len(entries)) + b"".join(k + v for k, v in entries)
    lenient_meta = unsorted_map([kv("embedding_dims", _uint(0)), kv("embedding_model", _text("")), kv("updated", _text("2026-10-19")),
                                 kv("created", _text("2026-10-19")), kv("name", _text("conformance"))])
    valid(
        "lenient_encoding", META, [rec(1, question=None)],
        "Keys in reverse order, `tokens` as a 2-byte integer, `id` with a 2-byte length head, `question` as null: "
        "all legal to read, none legal to write. A reader accepts them; a writer's output differs.",
        data=assemble(lenient_meta, [unsorted_map(lenient)]), reproducible=False,
    )

    # ── invalid: not a file of ours, or cut short ────────────────────────
    good = fds.encode(META, [rec(1), rec(2), rec(3)])
    invalid("not_fds_text", b"This is a text file, not a database.\n" * 4, "not_fds", "Plain text.")
    invalid("not_fds_png", b"\x89PNG\r\n\x1a\n" + bytes(100), "not_fds",
            "A PNG: its magic is the same shape as ours and differs at byte 1.")
    invalid("empty_file", b"", "truncated", "Zero bytes: an interrupted copy, not a foreign file.")
    invalid("cut_in_magic", fds.MAGIC[:5], "truncated", "The first 5 bytes of a good file.")
    invalid("cut_in_header", good[:40], "truncated", "A good file cut inside the header.")
    invalid("cut_in_metadata", good[:70], "truncated", "A good file cut inside the metadata.")
    invalid("cut_before_index", good[:-10], "truncated", "A good file missing the last 10 bytes of its index.")
    lying = bytearray(good)
    struct.pack_into("<I", lying, 20, 1_000_000)
    invalid("record_count_too_big", seal(bytes(lying), index=False), "truncated",
            "A header (with a correct CRC) that claims a million records: the index would run past the "
            "end of the file. It must be an error, not an allocation.")

    # ── invalid: unknown major ───────────────────────────────────────────
    for major in (5, 3):
        b = bytearray(good)
        struct.pack_into("<H", b, 8, major)
        invalid(f"major_{major}", seal(bytes(b), index=False), "unsupported_major",
                f"The magic of this format, with major version {major}, and a correct header CRC.")

    # ── invalid: damage found by a CRC ───────────────────────────────────
    def flipped(at):
        b = bytearray(good)
        b[at] ^= 0x40
        return bytes(b)

    meta_len = struct.unpack_from("<I", good, 32)[0]
    index_off = struct.unpack_from("<Q", good, 36)[0]
    first_record = fds.HEADER_LEN + meta_len
    second_record = struct.unpack_from("<Q", good, index_off + 16)[0]

    invalid("header_flip", flipped(21), "checksum", "One bit flipped in the record count.")
    invalid("header_crc_flip", flipped(61), "checksum", "One bit flipped in the header's own CRC.")
    invalid("metadata_flip", flipped(fds.HEADER_LEN + 3), "checksum", "One bit flipped in the metadata.")
    invalid("index_flip", flipped(index_off + 1), "checksum", "One bit flipped in the index.")
    invalid("record_flip", flipped(second_record + 5), "checksum",
            "One bit flipped inside the second of three records. The file opens; reading everything fails; "
            "records 0 and 2 are still readable one by one.", stage="record", readable=[0, 2])

    # ── invalid: the layout lies, with every CRC correct ─────────────────
    b = bytearray(good)
    struct.pack_into("<Q", b, 24, 10)
    invalid("metadata_inside_header", seal(bytes(b), index=False), "malformed", "meta_offset 10: inside the header.")
    b = bytearray(good)
    struct.pack_into("<Q", b, 36, fds.HEADER_LEN + 2)
    invalid("index_inside_metadata", seal(bytes(b), index=False), "malformed",
            "index_offset points into the metadata.")
    b = bytearray(good)
    struct.pack_into("<Q", b, index_off + 16, 4)  # record 1 starts inside the header
    invalid("record_outside_region", seal(bytes(b)), "malformed",
            "Index entry 1 points into the header; the index CRC is correct.", stage="record", readable=[0, 2])
    b = bytearray(good)
    struct.pack_into("<I", b, index_off + 16 + 12, 0xDEADBEEF)  # record 1's CRC in the index
    invalid("record_crc_wrong_in_index", seal(bytes(b)), "checksum",
            "Index entry 1 carries the wrong CRC for its record; the index CRC is correct.",
            stage="record", readable=[0, 2])

    # ── invalid: metadata that is not a map ──────────────────────────────
    invalid("metadata_not_a_map", assemble(_uint(5), []), "malformed", "The metadata is the integer 5.")
    invalid("metadata_trailing_bytes", assemble(fds.encode_meta(META) + b"\x00", []), "malformed",
            "A byte after the metadata map.")

    # ── invalid: records that break the rules (CRCs all correct) ─────────
    ok = _map(record_entries(rec(1)) + [kv("question", _text("q"))])

    def bad_record(name, blob, why):
        invalid(name, assemble(fds.encode_meta(META), [ok, blob]), "malformed", why, stage="record", readable=[0])

    base = record_entries(rec(2))

    def without(key):
        return [e for e in base if e[0] != _text(key)]

    def replacing(key, value):
        return [(k, value if k == _text(key) else v) for k, v in base]

    bad_record("record_unknown_kind", _map(replacing("kind", _text("fakt"))), "kind is `fakt`.")
    bad_record("record_kind_not_text", _map(replacing("kind", _uint(1))), "kind is an integer.")
    bad_record("record_missing_title", _map(without("title")), "The required key `title` is absent.")
    bad_record("record_missing_id", _map(without("id")), "The required key `id` is absent.")
    bad_record("record_tokens_is_text", _map(replacing("tokens", _text("2"))), "tokens is text.")
    bad_record("record_tokens_too_big", _map(replacing("tokens", _uint(2**32))), "tokens does not fit in 32 bits.")
    bad_record("record_bad_utf8", _map(replacing("title", _head(3, 2) + b"\xc3\x28")), "title is not valid UTF-8.")
    bad_record("record_embedding_5_bytes", _map(base + [kv("embedding", _bytes(b"\x00" * 5))]),
               "embedding is 5 bytes, not a multiple of 4.")
    bad_record("record_embedding_not_bytes", _map(base + [kv("embedding", _array([_uint(1)]))]),
               "embedding is an array, not a byte string.")
    bad_record("record_attr_value_not_text", _map(base + [kv("attrs", _map([(_text("k"), _uint(1))]))]),
               "An attribute value is an integer.")
    bad_record("record_link_not_text", _map(base + [kv("links", _array([_uint(1)]))]), "A link is an integer.")
    bad_record("record_trailing_bytes", _map(base) + b"\x00", "A byte after the record's map.")
    bad_record("record_not_a_map", _array([_text("id")]), "The record is an array.")

    # A key that appears twice. Written by hand: a map encoder would not.
    dup = _head(5, len(base) + 1) + b"".join(k + v for k, v in base) + _text("id") + _text("again")
    bad_record("record_duplicate_key", dup, "`id` appears twice.")
    # An unknown key twice is not an error in itself; a known one is.
    attrs_dup = _map(base + [kv("attrs", _head(5, 2) + _text("k") + _text("1") + _text("k") + _text("2"))])
    bad_record("record_duplicate_attribute", attrs_dup, "The attribute `k` appears twice.")

    # CBOR the format does not allow: indefinite length.
    indef = bytes([0xBF]) + b"".join(k + v for k, v in base) + bytes([0xFF])
    bad_record("record_indefinite_map", indef, "A map with indefinite length (0xBF ... 0xFF).")
    indef_text = _map(replacing("title", bytes([0x7F, 0x61, 0x61, 0xFF])))
    bad_record("record_indefinite_text", indef_text, "A text string with indefinite length.")

    # Hostile unknown values must be refused, not followed.
    deep = bytes([0x81]) * 64 + bytes([0x00])
    bad_record("record_unknown_key_too_deep", _map(base + [kv("zz", deep)]),
               "An unknown key whose value is nested 64 arrays deep (limit 32).")
    bad_record("record_indefinite_in_unknown_key", _map(base + [kv("zz", bytes([0x9F, 0x01, 0xFF]))]),
               "An unknown key whose value is an indefinite-length array: skipping does not excuse it (R3).")
    huge = _map(base + [kv("zz", bytes([0x9B]) + b"\xff" * 8)])
    bad_record("record_unknown_key_huge_array", huge, "An unknown key whose value claims 2^64-1 elements.")
    cut = _map(base)[:-3]
    bad_record("record_item_runs_past_end", cut, "The record's last item is cut short inside its own bytes.")

    # ── the guard's audit chain (SPEC §11.4) ─────────────────────────────
    import hashlib

    def chain(entries):
        lines, prev = [], "0" * 64
        for n, e in enumerate(entries, 1):
            line = json.dumps({**e, "n": n, "prev": prev}, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
            lines.append(line)
            prev = hashlib.sha256(line.encode("utf-8")).hexdigest()
        return lines, prev

    events = [
        {"at": "2026-10-23T10:00:00Z", "agent": "claude-r", "tool": "set_temperature", "decision": "allow"},
        {"at": "2026-10-23T10:00:01Z", "agent": "claude-r", "tool": "set_temperature", "decision": "deny", "reason": "Below -20 °C the stock is damaged."},
        {"at": "2026-10-23T10:00:02Z", "agent": "claude-r", "tool": "run_sql_drop", "decision": "deny"},
        {"at": "2026-10-23T10:00:03Z", "agent": "claude-e", "tool": "list_things", "decision": "allow"},
    ]
    good, head = chain(events)
    audit_dir = os.path.join(CASES, "audit")
    os.makedirs(audit_dir, exist_ok=True)

    def audit_case(name, lines, intact, broken_at, why):
        with open(os.path.join(audit_dir, name + ".jsonl"), "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(lines) + "\n")
        return {"file": name + ".jsonl", "intact": intact, "broken_at": broken_at, "description": why}

    edited = list(good)
    edited[1] = edited[1].replace('"deny"', '"allow"', 1)
    removed = [good[0], good[2], good[3]]
    inserted = [good[0], good[1], good[1], good[2], good[3]]
    swapped = [good[0], good[2], good[1], good[3]]
    notjson = good[:2] + ["{not json"] + good[3:]
    audit_cases = [
        audit_case("good", good, True, None, "A chain of four entries: n counts from 1, prev is the SHA-256 of the line before, zeros for the first."),
        audit_case("edited", edited, False, 3, "Line 2 changed afterwards: line 3 no longer matches it, and the break is reported there."),
        audit_case("removed", removed, False, 2, "Line 2 removed: the numbering skips."),
        audit_case("inserted", inserted, False, 3, "A line repeated: its number is wrong where it was put."),
        audit_case("swapped", swapped, False, 2, "Two lines exchanged."),
        audit_case("not_json", notjson, False, 3, "A line that is not JSON."),
    ]
    with open(os.path.join(audit_dir, "expected.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump({"head_of_good": head, "cases": audit_cases}, f, indent=2)
        f.write("\n")

    # ── manifest ─────────────────────────────────────────────────────────
    names = {g: sorted(f[:-4] for f in os.listdir(os.path.join(CASES, g)) if f.endswith(".fds")) for g in ("valid", "invalid")}
    with open(os.path.join(CASES, "MANIFEST.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(names, f, indent=2)
        f.write("\n")
    print(f"{len(names['valid'])} valid and {len(names['invalid'])} invalid cases in {CASES}")


if __name__ == "__main__":
    main()
