//! `serde` integration for [`Value`] and [`Document`], behind the `serde` feature.
//!
//! [`Value`] maps onto the serde data model the same way a dynamic JSON value
//! does: each variant serialises as its natural counterpart, and deserialising
//! reconstructs the closest variant from whatever the format produced. This is
//! what lets a caller move documents in and out of JSON, MessagePack, or any
//! other `serde` format without a bespoke conversion layer.
//!
//! [`Document`] serialises as a map and deserialises from one, preserving field
//! order as the underlying format allows.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::value::{Document, MAX_DOCUMENT_FIELDS, MAX_NESTING_DEPTH, Value};

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Int(n) => serializer.serialize_i64(*n),
            Value::Float(f) => serializer.serialize_f64(*f),
            Value::Str(s) => serializer.serialize_str(s),
            Value::Bytes(b) => serializer.serialize_bytes(b),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Object(doc) => doc.serialize(serializer),
        }
    }
}

impl Serialize for Document {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Most elements a visitor reserves from a format's size hint. Binary formats
/// report the length prefix of the input as the hint, so it is untrusted: the
/// collection grows past this as elements actually arrive.
const MAX_PREALLOC: usize = 4096;

/// Caps a format's size hint before it is used to reserve memory.
#[inline]
fn cautious(hint: Option<usize>) -> usize {
    hint.unwrap_or(0).min(MAX_PREALLOC)
}

/// Visitor that turns any serde value into the closest [`Value`] variant.
///
/// `depth` counts the arrays and objects enclosing the value being visited,
/// so nesting past [`MAX_NESTING_DEPTH`] fails cleanly instead of exhausting
/// the stack.
struct ValueVisitor {
    depth: usize,
}

impl ValueVisitor {
    /// The depth of a container visited here, or an error past the limit.
    fn inner_depth<E: de::Error>(&self) -> Result<usize, E> {
        let inner = self.depth + 1;
        if inner > MAX_NESTING_DEPTH {
            return Err(E::custom("nesting too deep for a bison-db document"));
        }
        Ok(inner)
    }
}

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any bison-db value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        i64::try_from(v)
            .map(Value::Int)
            .map_err(|_| E::custom("integer out of i64 range"))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::Str(String::from(v)))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::Str(v))
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Value, E> {
        Ok(Value::Bytes(v.to_vec()))
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Value, E> {
        Ok(Value::Bytes(v))
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        ValueSeed { depth: self.depth }.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let depth = self.inner_depth()?;
        let mut items = Vec::with_capacity(cautious(seq.size_hint()));
        while let Some(item) = seq.next_element_seed(ValueSeed { depth })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Value, A::Error> {
        let depth = self.inner_depth()?;
        Ok(Value::Object(document_from_map(map, depth)?))
    }
}

/// Deserializes one [`Value`] at a known nesting depth.
struct ValueSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for ValueSeed {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(ValueVisitor { depth: self.depth })
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ValueSeed { depth: 0 }.deserialize(deserializer)
    }
}

/// Visitor that builds a [`Document`] from a serde map.
struct DocumentVisitor;

impl<'de> Visitor<'de> for DocumentVisitor {
    type Value = Document;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map of document fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Document, A::Error> {
        document_from_map(map, 0)
    }
}

/// Drains a [`MapAccess`] into a [`Document`], used by both visitors above.
/// `depth` counts the containers enclosing the document's field values.
///
/// Fields are appended and duplicate keys collapsed once at the end, keeping
/// the build linear in the field count; a map wider than
/// [`MAX_DOCUMENT_FIELDS`] is refused.
fn document_from_map<'de, A: MapAccess<'de>>(
    mut map: A,
    depth: usize,
) -> Result<Document, A::Error> {
    let mut doc = Document::with_capacity(cautious(map.size_hint()));
    let mut count = 0usize;
    while let Some(key) = map.next_key::<String>()? {
        count += 1;
        if count > MAX_DOCUMENT_FIELDS {
            return Err(de::Error::custom("too many fields for a bison-db document"));
        }
        let value = map.next_value_seed(ValueSeed { depth })?;
        doc.push_field(key, value);
    }
    doc.dedup_last_wins();
    Ok(doc)
}

impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(DocumentVisitor)
    }
}

#[cfg(all(test, feature = "std"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::{Document, Value};

    #[test]
    fn test_value_json_roundtrip() {
        let mut doc = Document::new();
        doc.set("name", "ada")
            .set("age", 36_i64)
            .set("active", true);
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn test_nested_value_roundtrip() {
        let v = Value::Array(vec![
            Value::from(1_i64),
            Value::from("two"),
            Value::Null,
            Value::from(3.5_f64),
        ]);
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    /// An iterator that claims `claimed` items and yields none, the way a
    /// binary format reports a hostile length prefix as its size hint.
    struct Lying(usize);

    impl Iterator for Lying {
        type Item = (i64, i64);
        fn next(&mut self) -> Option<(i64, i64)> {
            None
        }
        fn size_hint(&self) -> (usize, Option<usize>) {
            (self.0, Some(self.0))
        }
    }

    struct LyingSeq(usize);

    impl Iterator for LyingSeq {
        type Item = i64;
        fn next(&mut self) -> Option<i64> {
            None
        }
        fn size_hint(&self) -> (usize, Option<usize>) {
            (self.0, Some(self.0))
        }
    }

    #[test]
    fn test_hostile_seq_size_hint_is_not_preallocated() {
        use serde::Deserialize;
        use serde::de::value::{Error, SeqDeserializer};
        // 2^32 - 1 elements, as a 5-byte MessagePack array32 header claims.
        let de = SeqDeserializer::<_, Error>::new(LyingSeq(u32::MAX as usize));
        let v = Value::deserialize(de).unwrap();
        assert_eq!(v, Value::Array(Vec::new()));
    }

    #[test]
    fn test_hostile_map_size_hint_is_not_preallocated() {
        use serde::Deserialize;
        use serde::de::value::{Error, MapDeserializer};
        let de = MapDeserializer::<_, Error>::new(Lying(u32::MAX as usize));
        let d = Document::deserialize(de).unwrap();
        assert!(d.is_empty());
    }
}
