//! From-scratch HTML parsing: tokenizer now, tree builder next.
//!
//! [`tokenize`] turns source text into a [`Token`] stream covering the core
//! WHATWG tokenizer productions (text + character references, tags with
//! attributes, comments, `DOCTYPE`, and the raw-text/RCDATA content models).
//! The tree-construction stage that builds a `janus-dom` tree from these tokens
//! lands next.

mod builder;
mod entities;
mod token;
mod tokenizer;

pub use builder::parse;
pub use entities::decode as decode_entities;
pub use token::{Attribute, Token};
pub use tokenizer::tokenize;
