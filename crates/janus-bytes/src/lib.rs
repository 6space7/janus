//! The byte-level entry point of the pipeline: URL parsing and MIME sniffing.
//!
//! Both are built from scratch (the from-scratch boundary keeps URL and
//! content-type policy in-engine — they are security-sensitive and define how
//! the loader treats a resource). [`Url`] implements RFC 3986 parsing and
//! reference resolution with WHATWG special-scheme normalization; [`mime`]
//! does magic-byte content sniffing.

mod percent;
mod url;

pub mod mime;

pub use percent::{percent_decode, percent_decode_str};
pub use url::{ParseError, Url};
