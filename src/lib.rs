//! # bison-db
//!
//! An embedded, document-oriented database for Rust - schemaless documents, secondary indexes, and ACID single-file storage, with zero network and zero external services.
//!
//! Scaffold release (v0.1.0). The public surface is being designed across the
//! 0.x series and frozen at v1.0. See `docs/API.md` and `dev/ROADMAP.md`.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(1 + 1, 2);
    }
}
