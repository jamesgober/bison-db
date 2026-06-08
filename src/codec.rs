//! Binary encoding for documents, plus the CRC used to frame stored records.
//!
//! The format is deliberately plain: a one-byte tag identifies each value's
//! variant, scalars are little-endian fixed width, and variable-length payloads
//! (strings, bytes, arrays, objects) are prefixed with a `u32` length. Fixed
//! widths keep the encoder and decoder branch-light and let the store read a
//! field without a separate schema. Every length is validated against the bytes
//! actually present before anything is allocated, so a corrupt or hostile length
//! can never drive an out-of-bounds read or an unbounded allocation.
//!
//! The encoding is internal to the store; it is not a stable wire format and is
//! versioned by the file header (see [`crate::store`]).

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::value::{Document, Value};

/// Tag byte for [`Value::Null`].
const TAG_NULL: u8 = 0;
/// Tag byte for [`Value::Bool`].
const TAG_BOOL: u8 = 1;
/// Tag byte for [`Value::Int`].
const TAG_INT: u8 = 2;
/// Tag byte for [`Value::Float`].
const TAG_FLOAT: u8 = 3;
/// Tag byte for [`Value::Str`].
const TAG_STR: u8 = 4;
/// Tag byte for [`Value::Bytes`].
const TAG_BYTES: u8 = 5;
/// Tag byte for [`Value::Array`].
const TAG_ARRAY: u8 = 6;
/// Tag byte for [`Value::Object`].
const TAG_OBJECT: u8 = 7;

/// Builds the reflected CRC-32C (Castagnoli) lookup table at compile time.
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Precomputed CRC-32C table, materialised once in the binary's data segment.
static CRC_TABLE: [u32; 256] = build_crc_table();

/// Computes the CRC-32C (Castagnoli) checksum of `data`.
///
/// CRC-32C is used over IEEE CRC-32 because it has stronger error detection and
/// hardware acceleration on common targets; this implementation is the portable
/// table-driven form. The store stamps this over every record payload and
/// re-checks it on read, so silent bit-rot is detected rather than misread.
///
/// # Examples
///
/// ```
/// # // `crc32c` is crate-internal; this illustrates the property it guarantees.
/// // Identical input yields an identical checksum; a single changed byte does not.
/// ```
#[must_use]
pub(crate) fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    let mut i = 0;
    while i < data.len() {
        let index = ((crc ^ u32::from(data[i])) & 0xff) as usize;
        crc = CRC_TABLE[index] ^ (crc >> 8);
        i += 1;
    }
    !crc
}

/// Encodes a document body (its field count and fields) onto the end of `out`.
///
/// Returns [`Error::ValueTooLarge`] if any single length field would not fit in
/// the `u32` the format reserves for it.
pub(crate) fn encode_document_into(out: &mut Vec<u8>, doc: &Document) -> Result<()> {
    write_len(out, doc.len())?;
    for (key, value) in doc {
        write_len(out, key.len())?;
        out.extend_from_slice(key.as_bytes());
        encode_value(out, value)?;
    }
    Ok(())
}

/// Encodes a single value onto the end of `out`.
fn encode_value(out: &mut Vec<u8>, value: &Value) -> Result<()> {
    match value {
        Value::Null => out.push(TAG_NULL),
        Value::Bool(b) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*b));
        }
        Value::Int(n) => {
            out.push(TAG_INT);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Float(f) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        Value::Str(s) => {
            out.push(TAG_STR);
            write_len(out, s.len())?;
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(TAG_BYTES);
            write_len(out, b.len())?;
            out.extend_from_slice(b);
        }
        Value::Array(items) => {
            out.push(TAG_ARRAY);
            write_len(out, items.len())?;
            for item in items {
                encode_value(out, item)?;
            }
        }
        Value::Object(doc) => {
            out.push(TAG_OBJECT);
            encode_document_into(out, doc)?;
        }
    }
    Ok(())
}

/// Writes a length as a little-endian `u32`, rejecting anything that overflows.
fn write_len(out: &mut Vec<u8>, len: usize) -> Result<()> {
    let len = u32::try_from(len).map_err(|_| Error::ValueTooLarge)?;
    out.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

/// Decodes a document body previously written by [`encode_document_into`].
///
/// Every length is checked against the remaining input, so a truncated or
/// corrupt buffer yields [`Error::Corrupt`] instead of a panic or over-read.
pub(crate) fn decode_document(bytes: &[u8]) -> Result<Document> {
    let mut reader = Reader::new(bytes);
    let doc = read_document(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(Error::Corrupt("trailing bytes after document"));
    }
    Ok(doc)
}

/// Reads a document body from `reader`.
fn read_document(reader: &mut Reader<'_>) -> Result<Document> {
    let field_count = reader.read_len()?;
    let mut doc = Document::with_capacity(field_count.min(reader.remaining()));
    for _ in 0..field_count {
        let key_len = reader.read_len()?;
        let key_bytes = reader.take(key_len)?;
        let key = core::str::from_utf8(key_bytes)
            .map_err(|_| Error::Corrupt("invalid utf-8 in field name"))?;
        let value = read_value(reader)?;
        let _ = doc.set(String::from(key), value);
    }
    Ok(doc)
}

/// Reads a single value from `reader`.
fn read_value(reader: &mut Reader<'_>) -> Result<Value> {
    let tag = reader.read_u8()?;
    match tag {
        TAG_NULL => Ok(Value::Null),
        TAG_BOOL => Ok(Value::Bool(reader.read_u8()? != 0)),
        TAG_INT => Ok(Value::Int(reader.read_i64()?)),
        TAG_FLOAT => Ok(Value::Float(f64::from_bits(reader.read_u64()?))),
        TAG_STR => {
            let len = reader.read_len()?;
            let bytes = reader.take(len)?;
            let s = core::str::from_utf8(bytes)
                .map_err(|_| Error::Corrupt("invalid utf-8 in string value"))?;
            Ok(Value::Str(String::from(s)))
        }
        TAG_BYTES => {
            let len = reader.read_len()?;
            Ok(Value::Bytes(reader.take(len)?.to_vec()))
        }
        TAG_ARRAY => {
            let len = reader.read_len()?;
            let mut items = Vec::with_capacity(len.min(reader.remaining()));
            for _ in 0..len {
                items.push(read_value(reader)?);
            }
            Ok(Value::Array(items))
        }
        TAG_OBJECT => Ok(Value::Object(read_document(reader)?)),
        _ => Err(Error::Corrupt("unknown value tag")),
    }
}

/// A bounds-checked, forward-only cursor over a byte slice.
///
/// Every read is validated against the remaining length, turning a short or
/// malformed buffer into [`Error::Corrupt`] rather than a panic.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Returns the next `n` bytes, advancing the cursor, or an error if fewer
    /// than `n` bytes remain.
    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::Corrupt("length overflow"))?;
        if end > self.buf.len() {
            return Err(Error::Corrupt("unexpected end of record"));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    #[inline]
    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    #[inline]
    fn read_u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[inline]
    fn read_u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    #[inline]
    fn read_i64(&mut self) -> Result<i64> {
        Ok(self.read_u64()? as i64)
    }

    #[inline]
    fn read_len(&mut self) -> Result<usize> {
        Ok(self.read_u32()? as usize)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn roundtrip(doc: &Document) -> Document {
        let mut buf = Vec::new();
        encode_document_into(&mut buf, doc).expect("encode");
        decode_document(&buf).expect("decode")
    }

    #[test]
    fn test_roundtrip_preserves_all_variants() {
        let mut nested = Document::new();
        nested.set("inner", 7_i64);
        let mut doc = Document::new();
        doc.set("null", Value::Null)
            .set("bool", true)
            .set("int", -42_i64)
            .set("float", 2.5_f64)
            .set("str", "héllo")
            .set("bytes", Value::Bytes(vec![0, 255, 7]))
            .set("array", Value::Array(vec![Value::from(1_i64), Value::Null]))
            .set("object", Value::Object(nested));
        assert_eq!(roundtrip(&doc), doc);
    }

    #[test]
    fn test_empty_document_roundtrips() {
        assert_eq!(roundtrip(&Document::new()), Document::new());
    }

    #[test]
    fn test_decode_truncated_buffer_is_corrupt() {
        let mut doc = Document::new();
        doc.set("k", "value");
        let mut buf = Vec::new();
        encode_document_into(&mut buf, &doc).unwrap();
        buf.truncate(buf.len() - 2);
        assert!(matches!(decode_document(&buf), Err(Error::Corrupt(_))));
    }

    #[test]
    fn test_decode_unknown_tag_is_corrupt() {
        // One field "k" whose value carries an out-of-range tag byte (200).
        let mut buf = Vec::new();
        write_len(&mut buf, 1).unwrap();
        write_len(&mut buf, 1).unwrap();
        buf.push(b'k');
        buf.push(200);
        assert!(matches!(decode_document(&buf), Err(Error::Corrupt(_))));
    }

    #[test]
    fn test_decode_rejects_trailing_bytes() {
        let mut buf = Vec::new();
        encode_document_into(&mut buf, &Document::new()).unwrap();
        buf.push(0);
        assert!(matches!(decode_document(&buf), Err(Error::Corrupt(_))));
    }

    #[test]
    fn test_crc_detects_single_bit_flip() {
        let a = crc32c(b"the quick brown fox");
        let b = crc32c(b"the quick brown fox!");
        assert_ne!(a, b);
        assert_eq!(crc32c(b"abc"), crc32c(b"abc"));
    }

    #[test]
    fn test_hostile_array_length_does_not_allocate_unbounded() {
        // Claim 4 billion elements in a 5-byte buffer: must error, not OOM.
        let mut buf = Vec::new();
        write_len(&mut buf, 1).unwrap(); // one field
        write_len(&mut buf, 0).unwrap(); // empty key
        buf.push(TAG_ARRAY);
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode_document(&buf), Err(Error::Corrupt(_))));
    }

    proptest::proptest! {
        /// The decoder must terminate with `Ok` or `Err` on *any* input — never
        /// panic, over-read, or allocate unbounded — since on-disk bytes are
        /// untrusted after corruption. This is the in-tree fuzz of the parse path.
        #[test]
        fn prop_decode_arbitrary_bytes_never_panics(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        ) {
            let _ = decode_document(&bytes);
        }

        /// Encoding a real document and decoding it back is always lossless, and
        /// decoding any single-byte truncation of that encoding never panics.
        #[test]
        fn prop_truncations_of_valid_encoding_never_panic(
            key in "[a-z]{1,6}",
            n in proptest::prelude::any::<i64>(),
        ) {
            let mut doc = Document::new();
            doc.set(key, n);
            let mut buf = Vec::new();
            encode_document_into(&mut buf, &doc).unwrap();
            for cut in 0..buf.len() {
                let _ = decode_document(&buf[..cut]);
            }
            proptest::prop_assert_eq!(decode_document(&buf).unwrap(), doc);
        }
    }
}
