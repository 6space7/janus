//! The token types produced by the [tokenizer](crate::tokenizer).

/// A name/value attribute on a start tag. Names are ASCII-lowercased; values
/// have had character references decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    /// The attribute name, ASCII-lowercased.
    pub name: String,
    /// The attribute value (character references already decoded).
    pub value: String,
}

/// A single HTML token.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Token {
    /// A `<!DOCTYPE …>` declaration.
    Doctype {
        /// The doctype name (e.g. `html`), if present.
        name: Option<String>,
        /// Whether the parser should switch to quirks mode.
        force_quirks: bool,
    },
    /// A start tag, e.g. `<div class="x">`.
    StartTag {
        /// The tag name, ASCII-lowercased.
        name: String,
        /// The attributes, in source order.
        attributes: Vec<Attribute>,
        /// Whether the tag was self-closing (`<br/>`).
        self_closing: bool,
    },
    /// An end tag, e.g. `</div>`.
    EndTag {
        /// The tag name, ASCII-lowercased.
        name: String,
    },
    /// A comment's text (between `<!--` and `-->`).
    Comment(String),
    /// A run of character data (character references already decoded, except in
    /// raw-text elements like `<script>`/`<style>`).
    Text(String),
    /// The end of the input stream.
    Eof,
}

impl Token {
    /// Convenience: the tag name if this is a start or end tag.
    #[must_use]
    pub fn tag_name(&self) -> Option<&str> {
        match self {
            Token::StartTag { name, .. } | Token::EndTag { name } => Some(name),
            _ => None,
        }
    }
}
