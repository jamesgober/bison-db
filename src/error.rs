//! Error type for `bison-db`.
//!
//! Every fallible operation in the crate returns [`Result<T>`], an alias for
//! `core::result::Result<T, Error>`. [`Error`] is a small, closed enum: each
//! variant names a distinct failure class so callers can match on the cause and
//! decide how to recover rather than parsing a message string.
//!
//! The type is hand-rolled rather than derived from a macro crate so it stays
//! dependency-free and compiles unchanged under `no_std` (only [`Error::Io`] is
//! gated behind the `std` feature, because it wraps [`std::io::Error`]).

use core::fmt;

/// A specialised [`Result`](core::result::Result) for `bison-db` operations.
///
/// Used throughout the public API so callers can rely on a single error type.
///
/// # Examples
///
/// ```
/// use bison_db::{Document, Result, Value};
///
/// fn name_of(doc: &Document) -> Result<&str> {
///     // `Value::as_str` borrows; the surrounding code propagates with `?`.
///     Ok(doc.get("name").and_then(Value::as_str).unwrap_or("<unknown>"))
/// }
/// # let mut d = Document::new();
/// # d.set("name", "bison");
/// # assert_eq!(name_of(&d).unwrap(), "bison");
/// ```
pub type Result<T> = core::result::Result<T, Error>;

/// The set of failures a `bison-db` operation can produce.
///
/// Variants are grouped by origin: I/O failures from the host filesystem, and
/// format failures detected while decoding the on-disk log. A format failure
/// always means the bytes on disk did not match the expectations encoded by the
/// writer — never that the caller passed bad arguments to an in-memory type.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An underlying filesystem operation failed.
    ///
    /// Wraps the originating [`std::io::Error`] so the caller can inspect the
    /// [`std::io::ErrorKind`] (for example, [`PermissionDenied`] when opening a
    /// read-only path, or [`NotFound`] for a missing parent directory).
    ///
    /// [`PermissionDenied`]: std::io::ErrorKind::PermissionDenied
    /// [`NotFound`]: std::io::ErrorKind::NotFound
    #[cfg(feature = "std")]
    Io(std::io::Error),

    /// The file does not begin with a valid `bison-db` header.
    ///
    /// Returned by [`Db::open`](crate::Db::open) when the target path exists, is
    /// non-empty, but its magic bytes do not identify it as a bison-db store —
    /// the usual cause of opening the wrong file by mistake.
    BadMagic,

    /// The on-disk format version is newer than this build understands.
    ///
    /// The contained value is the version stamped in the file header. A binary
    /// built against an older release will refuse to read a file written by a
    /// newer one rather than risk misinterpreting it.
    UnsupportedVersion(u16),

    /// A stored record failed its integrity check or was structurally invalid.
    ///
    /// This covers a CRC mismatch, an unknown value tag, a length field that
    /// overruns the record, or a non-UTF-8 string — any signal that the bytes
    /// were corrupted in place after being written. A clean torn write at the
    /// very end of the log is *not* reported here: it is recovered silently by
    /// truncating the partial tail (see [`Db::open`](crate::Db::open)).
    ///
    /// The contained `&'static str` is a fixed diagnostic label, never a
    /// formatted message, so producing it allocates nothing.
    Corrupt(&'static str),

    /// A value was too large to encode within the configured record limit.
    ///
    /// Guards against a single document growing past
    /// [`MAX_RECORD_BYTES`](crate::MAX_RECORD_BYTES), which would otherwise
    /// force an unbounded allocation on the read path during recovery.
    ValueTooLarge,
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    #[inline]
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "std")]
            Error::Io(err) => write!(f, "i/o error: {err}"),
            Error::BadMagic => f.write_str("not a bison-db file: header magic did not match"),
            Error::UnsupportedVersion(v) => {
                write!(
                    f,
                    "on-disk format version {v} is newer than this build supports"
                )
            }
            Error::Corrupt(what) => write!(f, "corrupt record: {what}"),
            Error::ValueTooLarge => f.write_str("value exceeds the maximum record size"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(not(feature = "std"))]
impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_corrupt_includes_label() {
        let err = Error::Corrupt("crc mismatch");
        assert!(err.to_string().contains("crc mismatch"));
    }

    #[test]
    fn test_display_unsupported_version_includes_number() {
        let err = Error::UnsupportedVersion(9);
        assert!(err.to_string().contains('9'));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_io_error_conversion_preserves_source() {
        use std::error::Error as _;
        let io = std::io::Error::other("disk on fire");
        let err: Error = io.into();
        assert!(err.source().is_some());
    }
}
