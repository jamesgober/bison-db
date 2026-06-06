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

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::value::{Document, Value};

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

/// Visitor that turns any serde value into the closest [`Value`] variant.
struct ValueVisitor;

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
        Value::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Value, A::Error> {
        Ok(Value::Object(document_from_map(map)?))
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
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
        document_from_map(map)
    }
}

/// Drains a [`MapAccess`] into a [`Document`], used by both visitors above.
fn document_from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Document, A::Error> {
    let mut doc = Document::with_capacity(map.size_hint().unwrap_or(0));
    while let Some((key, value)) = map.next_entry::<String, Value>()? {
        let _ = doc.set(key, value);
    }
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
}
