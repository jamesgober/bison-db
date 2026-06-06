//! The document model: [`Value`] and [`Document`].
//!
//! A [`Document`] is an ordered set of named fields — the unit a store holds.
//! Each field's content is a [`Value`], a small tagged union covering the
//! JSON-like shapes a schemaless record needs: null, boolean, signed integer,
//! float, UTF-8 string, raw bytes, array, and nested document.
//!
//! Fields keep insertion order. Two documents are equal when they carry the
//! same fields in the same order, so encode/decode round-trips compare equal.
//! Order is preserved because it is cheap to maintain over a flat vector and
//! because a stable field order keeps the on-disk encoding deterministic.

use alloc::string::String;
use alloc::vec::Vec;

/// A single field value inside a [`Document`].
///
/// `Value` is deliberately small and closed: it models the shapes a schemaless
/// document store needs and nothing more. Nesting is expressed through
/// [`Value::Array`] and [`Value::Object`], so an arbitrarily deep record is
/// just a tree of `Value`s.
///
/// # Examples
///
/// ```
/// use bison_db::Value;
///
/// let v = Value::from("hello");
/// assert_eq!(v.as_str(), Some("hello"));
/// assert!(Value::from(42_i64).as_int() == Some(42));
/// assert!(Value::Null.is_null());
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// The absence of a value.
    #[default]
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer. All integral fields are stored at this width.
    Int(i64),
    /// A 64-bit IEEE-754 float.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
    /// An opaque byte string, for binary fields that are not valid UTF-8.
    Bytes(Vec<u8>),
    /// An ordered list of values.
    Array(Vec<Value>),
    /// A nested document.
    Object(Document),
}

impl Value {
    /// Returns `true` if this value is [`Value::Null`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert!(Value::Null.is_null());
    /// assert!(!Value::from(0_i64).is_null());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns the boolean if this is a [`Value::Bool`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::from(true).as_bool(), Some(true));
    /// assert_eq!(Value::from(1_i64).as_bool(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the integer if this is a [`Value::Int`], otherwise `None`.
    ///
    /// Floats are not coerced; a [`Value::Float`] returns `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::from(7_i64).as_int(), Some(7));
    /// assert_eq!(Value::from(7.0_f64).as_int(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns the float if this is a [`Value::Float`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::from(1.5_f64).as_float(), Some(1.5));
    /// assert_eq!(Value::from(1_i64).as_float(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Returns the string slice if this is a [`Value::Str`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::from("bison").as_str(), Some("bison"));
    /// ```
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the byte slice if this is a [`Value::Bytes`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::Bytes(vec![1, 2, 3]).as_bytes(), Some(&[1, 2, 3][..]));
    /// ```
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    /// Returns the element slice if this is a [`Value::Array`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// let v = Value::Array(vec![Value::from(1_i64), Value::from(2_i64)]);
    /// assert_eq!(v.as_array().map(<[_]>::len), Some(2));
    /// ```
    #[inline]
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    /// Returns the nested document if this is a [`Value::Object`], otherwise `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{Document, Value};
    /// let mut inner = Document::new();
    /// inner.set("k", 1_i64);
    /// let v = Value::Object(inner);
    /// assert_eq!(v.as_object().and_then(|d| d.get("k")).and_then(Value::as_int), Some(1));
    /// ```
    #[inline]
    #[must_use]
    pub fn as_object(&self) -> Option<&Document> {
        match self {
            Value::Object(d) => Some(d),
            _ => None,
        }
    }

    /// Returns a short, stable name for the value's variant.
    ///
    /// Intended for diagnostics and error messages, not for logic; match on the
    /// variant directly when behaviour depends on the type.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Value;
    /// assert_eq!(Value::from(1_i64).type_name(), "int");
    /// assert_eq!(Value::Null.type_name(), "null");
    /// ```
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

impl From<bool> for Value {
    #[inline]
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i32> for Value {
    #[inline]
    fn from(v: i32) -> Self {
        Value::Int(i64::from(v))
    }
}

impl From<i64> for Value {
    #[inline]
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<u32> for Value {
    #[inline]
    fn from(v: u32) -> Self {
        Value::Int(i64::from(v))
    }
}

impl From<f64> for Value {
    #[inline]
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<&str> for Value {
    #[inline]
    fn from(v: &str) -> Self {
        Value::Str(String::from(v))
    }
}

impl From<String> for Value {
    #[inline]
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}

impl From<Vec<u8>> for Value {
    #[inline]
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v)
    }
}

impl From<Vec<Value>> for Value {
    #[inline]
    fn from(v: Vec<Value>) -> Self {
        Value::Array(v)
    }
}

impl From<Document> for Value {
    #[inline]
    fn from(v: Document) -> Self {
        Value::Object(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    /// `Some(x)` becomes `x`'s value; `None` becomes [`Value::Null`].
    #[inline]
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}

/// An ordered collection of named fields — the record a store holds.
///
/// A document maps `String` keys to [`Value`]s and preserves the order in which
/// fields were first inserted. Lookups are a linear scan, which is the fastest
/// strategy for the small field counts typical of documents: it keeps the keys
/// contiguous in memory and avoids the hashing and pointer-chasing overhead a
/// map would add at this size.
///
/// # Examples
///
/// ```
/// use bison_db::Document;
///
/// let mut user = Document::new();
/// user.set("name", "ada").set("born", 1815_i64);
///
/// assert_eq!(user.len(), 2);
/// assert_eq!(user.get("name").and_then(|v| v.as_str()), Some("ada"));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    fields: Vec<(String, Value)>,
}

impl Document {
    /// Creates an empty document.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let doc = Document::new();
    /// assert!(doc.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Document { fields: Vec::new() }
    }

    /// Creates an empty document with room for `capacity` fields before it
    /// needs to reallocate.
    ///
    /// Use this when the field count is known up front to avoid intermediate
    /// growth allocations.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let doc = Document::with_capacity(4);
    /// assert!(doc.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Document {
            fields: Vec::with_capacity(capacity),
        }
    }

    /// Sets `key` to `value`, returning `&mut self` so calls can be chained.
    ///
    /// If `key` is already present its value is replaced in place, preserving
    /// the field's original position. Otherwise the field is appended.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("a", 1_i64).set("b", 2_i64).set("a", 3_i64);
    /// // "a" keeps its leading position but takes the new value.
    /// assert_eq!(doc.keys().collect::<Vec<_>>(), ["a", "b"]);
    /// assert_eq!(doc.get("a").and_then(|v| v.as_int()), Some(3));
    /// ```
    pub fn set<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<Value>,
    {
        let key = key.into();
        let value = value.into();
        match self.fields.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((key, value)),
        }
        self
    }

    /// Returns a reference to the value for `key`, or `None` if absent.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("k", "v");
    /// assert_eq!(doc.get("k").and_then(|v| v.as_str()), Some("v"));
    /// assert!(doc.get("missing").is_none());
    /// ```
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Returns a mutable reference to the value for `key`, or `None` if absent.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{Document, Value};
    /// let mut doc = Document::new();
    /// doc.set("n", 1_i64);
    /// if let Some(Value::Int(n)) = doc.get_mut("n") {
    ///     *n += 41;
    /// }
    /// assert_eq!(doc.get("n").and_then(|v| v.as_int()), Some(42));
    /// ```
    #[must_use]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.fields
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Returns `true` if `key` is present.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("k", 1_i64);
    /// assert!(doc.contains_key("k"));
    /// assert!(!doc.contains_key("other"));
    /// ```
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.fields.iter().any(|(k, _)| k == key)
    }

    /// Removes `key`, returning its value if it was present.
    ///
    /// Remaining fields keep their relative order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("a", 1_i64).set("b", 2_i64);
    /// assert_eq!(doc.remove("a").and_then(|v| v.as_int()), Some(1));
    /// assert!(!doc.contains_key("a"));
    /// assert_eq!(doc.keys().collect::<Vec<_>>(), ["b"]);
    /// ```
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.fields.iter().position(|(k, _)| k == key)?;
        Some(self.fields.remove(idx).1)
    }

    /// Returns the number of fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("a", 1_i64);
    /// assert_eq!(doc.len(), 1);
    /// ```
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` if the document has no fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// assert!(Document::new().is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Removes all fields, keeping the allocated capacity for reuse.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("a", 1_i64);
    /// doc.clear();
    /// assert!(doc.is_empty());
    /// ```
    #[inline]
    pub fn clear(&mut self) {
        self.fields.clear();
    }

    /// Returns an iterator over the fields as `(&str, &Value)` pairs, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("a", 1_i64).set("b", 2_i64);
    /// let collected: Vec<_> = doc.iter().map(|(k, _)| k).collect();
    /// assert_eq!(collected, ["a", "b"]);
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns an iterator over the field keys, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("x", 1_i64);
    /// assert_eq!(doc.keys().collect::<Vec<_>>(), ["x"]);
    /// ```
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|(k, _)| k.as_str())
    }

    /// Returns an iterator over the field values, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::Document;
    /// let mut doc = Document::new();
    /// doc.set("x", 9_i64);
    /// assert_eq!(doc.values().filter_map(|v| v.as_int()).sum::<i64>(), 9);
    /// ```
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.fields.iter().map(|(_, v)| v)
    }
}

impl<'a> IntoIterator for &'a Document {
    type Item = (&'a str, &'a Value);
    type IntoIter = core::iter::Map<
        core::slice::Iter<'a, (String, Value)>,
        fn(&'a (String, Value)) -> (&'a str, &'a Value),
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn pair(kv: &(String, Value)) -> (&str, &Value) {
            (kv.0.as_str(), &kv.1)
        }
        self.fields.iter().map(pair)
    }
}

impl<K, V> FromIterator<(K, V)> for Document
where
    K: Into<String>,
    V: Into<Value>,
{
    /// Builds a document from key/value pairs. Later duplicates overwrite
    /// earlier ones, matching [`Document::set`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bison_db::{Document, Value};
    /// let doc: Document = [("a", 1_i64), ("b", 2_i64)].into_iter().collect();
    /// assert_eq!(doc.len(), 2);
    /// ```
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut doc = Document::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            let _ = doc.set(k, v);
        }
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_existing_key_replaces_in_place() {
        let mut doc = Document::new();
        doc.set("a", 1_i64).set("b", 2_i64).set("a", 9_i64);
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(doc.get("a").and_then(Value::as_int), Some(9));
    }

    #[test]
    fn test_remove_absent_key_returns_none() {
        let mut doc = Document::new();
        doc.set("a", 1_i64);
        assert!(doc.remove("missing").is_none());
        assert_eq!(doc.len(), 1);
    }

    #[test]
    fn test_remove_preserves_order() {
        let mut doc = Document::new();
        doc.set("a", 1_i64).set("b", 2_i64).set("c", 3_i64);
        let _ = doc.remove("b");
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["a", "c"]);
    }

    #[test]
    fn test_value_accessors_reject_wrong_variant() {
        assert!(Value::from(1_i64).as_str().is_none());
        assert!(Value::from("x").as_int().is_none());
        assert!(Value::Null.as_bool().is_none());
    }

    #[test]
    fn test_from_option_maps_none_to_null() {
        let none: Option<i64> = None;
        assert!(Value::from(none).is_null());
        assert_eq!(Value::from(Some(5_i64)).as_int(), Some(5));
    }

    #[test]
    fn test_equality_is_order_sensitive() {
        let mut a = Document::new();
        a.set("x", 1_i64).set("y", 2_i64);
        let mut b = Document::new();
        b.set("y", 2_i64).set("x", 1_i64);
        assert_ne!(a, b);
    }
}
