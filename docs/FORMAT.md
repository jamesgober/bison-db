<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>bison-db</b><br>
    <sub><sup>ON-DISK FORMAT</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./API.md" title="API Reference"><b>API</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>FORMAT</span>
        <span>&nbsp;│&nbsp;</span>
        <a href="../CHANGELOG.md" title="Changelog"><b>CHANGELOG</b></a>
    </sup>
</div>
<br>

> The byte-level layout of a bison-db store file.
> **Frozen as of v0.4.0 (format version 1).** A file written by bison-db 0.2.0 or
> later is readable by every later release; an incompatible change to this layout
> would bump the format version and the crate's major version together.

## Overview

A bison-db database is a single file: a fixed-size **header** followed by a
**log** of variable-length **records**. The file is append-only — every insert,
overwrite, and delete adds a record at the end; nothing is ever rewritten in
place. The live state of the database is the result of replaying the log from
the start.

All multi-byte integers are **little-endian**, regardless of host architecture,
so a file is portable across platforms. Lengths and offsets are unsigned.

```
+------------------+
|  Header (16 B)   |
+------------------+
|  Record 0        |
+------------------+
|  Record 1        |
+------------------+
|  ...             |
+------------------+
|  Record N        |   <- the tail; new records append here
+------------------+
```

## Header

The first 16 bytes of the file.

| Offset | Size | Field | Value |
|-------:|-----:|-------|-------|
| 0 | 8 | Magic | ASCII `BISONDB1` (`0x42 0x49 0x53 0x4F 0x4E 0x44 0x42 0x31`) |
| 8 | 2 | Format version | `u16` LE, currently `1` |
| 10 | 6 | Reserved | zero-filled; reserved for future flags |

Opening a non-empty file whose first 8 bytes are not the magic fails with
`BadMagic`. A format version greater than the running build's supported version
fails with `UnsupportedVersion`; the 6 reserved bytes allow additive, backward
-compatible evolution without a version bump.

## Records

Each record is an 8-byte **frame** followed by its **payload**.

```
+--------+--------+----------------------------------+
| len:u32| crc:u32|        payload (len bytes)       |
+--------+--------+----------------------------------+
   LE       LE
```

| Field | Size | Description |
|-------|-----:|-------------|
| `len` | 4 | `u32` LE — length of the payload in bytes |
| `crc` | 4 | `u32` LE — CRC-32C (Castagnoli) of the payload |
| `payload` | `len` | the operation, document id, and (for writes) the body |

`len` is at least 9 (a one-byte op tag plus an 8-byte id) and at most
`MAX_RECORD_BYTES` (64 MiB). The CRC is computed over exactly the `payload`
bytes.

### Payload

```
+------+-------------+-----------------------------+
| op:1 |   id:u64    |   body (PUT only)           |
+------+-------------+-----------------------------+
          LE
```

| Field | Size | Description |
|-------|-----:|-------------|
| `op` | 1 | operation tag: `1` = PUT (insert/overwrite), `2` = DELETE |
| `id` | 8 | `u64` LE — the document id |
| `body` | rest | the encoded document — **present for PUT only** |

A DELETE payload is exactly 9 bytes (op + id); it is a tombstone with no body.

## Document body encoding

A document body is a field count followed by that many `(key, value)` pairs:

```
count:u32  ( keylen:u32  key:bytes  value )*
```

- `count` — `u32` LE number of fields.
- For each field: `keylen` (`u32` LE), the UTF-8 key bytes, then the encoded
  value.

A **value** is a one-byte tag followed by a tag-specific payload:

| Tag | Type | Payload |
|----:|------|---------|
| 0 | Null | none |
| 1 | Bool | 1 byte: `0` or `1` |
| 2 | Int | `i64` LE (8 bytes) |
| 3 | Float | `f64` bit pattern, LE (8 bytes) |
| 4 | Str | `len:u32` LE, then `len` UTF-8 bytes |
| 5 | Bytes | `len:u32` LE, then `len` raw bytes |
| 6 | Array | `len:u32` LE element count, then `len` values |
| 7 | Object | a nested document body (as above) |

Arrays and objects nest recursively. Every length is validated against the bytes
remaining in the record before any allocation, so a corrupt or hostile length is
rejected rather than driving an over-read.

## Reading and recovery

A reader replays records from offset 16 to the end of the file:

1. Read the 8-byte frame. If fewer than 8 bytes remain, stop.
2. If `len` is out of range (`< 9` or `> MAX_RECORD_BYTES`), or the record would
   extend past end-of-file, treat it as a torn tail and stop.
3. Read `len` payload bytes and verify the CRC.
   - On mismatch **at the end of the file**, it is a torn final write: drop it
     and stop.
   - On mismatch **before** the end of the file, the file is corrupt
     (`Corrupt`).
4. Apply the record: a PUT records the id and body location; a DELETE removes the
   id.

After replay, any trailing bytes beyond the last good record (a torn tail) are
truncated, returning the file to its last consistent state. The result is an
in-memory map from live document id to the byte offset of its most recent body.

## Durability

Writes reach the OS page cache immediately. When they are forced to the physical
device depends on the store's sync policy:

- **Always** — `fsync` after every record.
- **Manual** (default) — `fsync` on an explicit flush and, best-effort, on close.

On a freshly created file the parent directory is `fsync`ed so the file's
existence is itself durable. A crash never tears a record that was already
durable; the worst case under Manual is the loss of the most recent unsynced
records, which replay then truncates cleanly.

## Not in the file

Secondary indexes are **not** stored in the file. They are an in-memory structure
rebuilt per session with `create_index`, which is what keeps this format small
and stable. Persisting indexes, if added, will use a separate sidecar file and
will not change the layout described here.

---

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
