//! A from-scratch HTML tokenizer.
//!
//! Covers the core productions of the WHATWG tokenizer: text + character
//! references, start/end tags with attributes (quoted, single-quoted, and
//! unquoted values), self-closing tags, comments, `DOCTYPE`, and the
//! raw-text/RCDATA content models for `<script>`/`<style>` and
//! `<title>`/`<textarea>`. It scans a `Vec<char>` and never panics on
//! malformed input — unterminated constructs run to end-of-input.
//!
//! Full state-machine fidelity (every error-recovery branch) and the
//! html5ever diff-oracle are tracked for later; this handles the real-world
//! shapes the P0 pipeline needs.

use crate::entities;
use crate::token::{Attribute, Token};

/// Tokenize `input` into a flat token stream ending in [`Token::Eof`].
#[must_use]
pub fn tokenize(input: &str) -> Vec<Token> {
    Tokenizer::new(input).run()
}

struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
    tokens: Vec<Token>,
    text: String,
}

impl Tokenizer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            tokens: Vec::new(),
            text: String::new(),
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.pos < self.chars.len() {
            if self.chars[self.pos] == '<' && self.starts_markup() {
                self.flush_text();
                self.consume_markup();
            } else {
                self.text.push(self.chars[self.pos]);
                self.pos += 1;
            }
        }
        self.flush_text();
        self.tokens.push(Token::Eof);
        self.tokens
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// `<` introduces markup only when followed by a name, `/`, `!`, or `?`.
    fn starts_markup(&self) -> bool {
        matches!(self.peek_at(1), Some(c) if c == '/' || c == '!' || c == '?' || c.is_ascii_alphabetic())
    }

    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            let decoded = entities::decode(&self.text);
            self.tokens.push(Token::Text(decoded));
            self.text.clear();
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn consume_markup(&mut self) {
        match self.peek_at(1) {
            Some('/') => self.consume_end_tag(),
            Some('!') => self.consume_declaration(),
            Some('?') => {
                self.pos += 1; // step onto '?'; treat the rest as a bogus comment
                self.consume_bogus_comment();
            }
            _ => self.consume_start_tag(),
        }
    }

    fn consume_tag_name(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '/' || c == '>' {
                break;
            }
            self.pos += 1;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase()
    }

    fn consume_start_tag(&mut self) {
        self.pos += 1; // consume '<'
        let name = self.consume_tag_name();
        let mut attributes: Vec<Attribute> = Vec::new();
        let mut self_closing = false;

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => break,
                Some('>') => {
                    self.pos += 1;
                    break;
                }
                Some('/') => {
                    if self.peek_at(1) == Some('>') {
                        self_closing = true;
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1; // stray slash
                }
                _ => {
                    if let Some(attr) = self.consume_attribute() {
                        if !attributes.iter().any(|a| a.name == attr.name) {
                            attributes.push(attr);
                        }
                    }
                }
            }
        }

        self.tokens.push(Token::StartTag {
            name: name.clone(),
            attributes,
            self_closing,
        });

        if !self_closing {
            if is_rawtext_element(&name) {
                self.consume_raw_content(&name, false);
            } else if is_rcdata_element(&name) {
                self.consume_raw_content(&name, true);
            }
        }
    }

    fn consume_attribute(&mut self) -> Option<Attribute> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '=' || c == '>' || c == '/' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            // No name consumed (e.g. a stray char); advance to avoid a stall.
            self.pos += 1;
            return None;
        }
        let name = self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();

        self.skip_whitespace();
        let value = if self.peek() == Some('=') {
            self.pos += 1;
            self.skip_whitespace();
            self.consume_attribute_value()
        } else {
            String::new()
        };
        Some(Attribute { name, value })
    }

    fn consume_attribute_value(&mut self) -> String {
        match self.peek() {
            Some(q @ ('"' | '\'')) => {
                self.pos += 1; // opening quote
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c == q {
                        break;
                    }
                    self.pos += 1;
                }
                let raw: String = self.chars[start..self.pos].iter().collect();
                if self.peek() == Some(q) {
                    self.pos += 1; // closing quote
                }
                entities::decode(&raw)
            }
            _ => {
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c.is_ascii_whitespace() || c == '>' {
                        break;
                    }
                    self.pos += 1;
                }
                let raw: String = self.chars[start..self.pos].iter().collect();
                entities::decode(&raw)
            }
        }
    }

    fn consume_end_tag(&mut self) {
        self.pos += 2; // consume '</'
        if !matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
            // `</>` or `</ …` — treat the remainder up to '>' as a bogus comment.
            self.consume_bogus_comment();
            return;
        }
        let name = self.consume_tag_name();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '>' {
                break;
            }
        }
        if !name.is_empty() {
            self.tokens.push(Token::EndTag { name });
        }
    }

    fn consume_declaration(&mut self) {
        // self.pos is at '<', self.pos+1 is '!'.
        if self.peek_at(2) == Some('-') && self.peek_at(3) == Some('-') {
            self.pos += 4; // consume '<!--'
            self.consume_comment();
        } else if self.matches_ignore_case(2, "doctype") {
            self.consume_doctype();
        } else {
            self.pos += 1; // step onto '!'; rest is a bogus comment
            self.consume_bogus_comment();
        }
    }

    fn consume_comment(&mut self) {
        let start = self.pos;
        let end = self
            .find_sequence(start, &['-', '-', '>'])
            .unwrap_or(self.chars.len());
        let content: String = self.chars[start..end].iter().collect();
        self.pos = (end + 3).min(self.chars.len());
        self.tokens.push(Token::Comment(content));
    }

    fn consume_bogus_comment(&mut self) {
        // self.pos is just after the introducer; read to the next '>'.
        let start = self.pos;
        let mut end = start;
        while let Some(c) = self.chars.get(end).copied() {
            if c == '>' {
                break;
            }
            end += 1;
        }
        let content: String = self.chars[start..end].iter().collect();
        self.pos = (end + 1).min(self.chars.len());
        self.tokens.push(Token::Comment(content));
    }

    fn consume_doctype(&mut self) {
        self.pos += 2; // consume '<!'
        let start = self.pos;
        let mut end = start;
        while let Some(c) = self.chars.get(end).copied() {
            if c == '>' {
                break;
            }
            end += 1;
        }
        let raw: String = self.chars[start..end].iter().collect();
        self.pos = (end + 1).min(self.chars.len());

        // `raw` looks like `doctype html …`; take the first word after the keyword.
        let lower = raw.to_ascii_lowercase();
        let after_keyword = lower.trim_start().strip_prefix("doctype").unwrap_or("");
        let name = after_keyword.split_whitespace().next().map(str::to_string);
        let force_quirks = name.as_deref() != Some("html");
        self.tokens.push(Token::Doctype { name, force_quirks });
    }

    /// Consume raw text up to the matching end tag (`</name`), emitting it as a
    /// single [`Token::Text`]. RCDATA elements decode character references;
    /// raw-text elements do not. Leaves `pos` at the end tag for [`run`] to
    /// tokenize normally.
    ///
    /// [`run`]: Tokenizer::run
    fn consume_raw_content(&mut self, name: &str, decode: bool) {
        let start = self.pos;
        let end = self.find_end_tag(name).unwrap_or(self.chars.len());
        if end > start {
            let raw: String = self.chars[start..end].iter().collect();
            let text = if decode { entities::decode(&raw) } else { raw };
            self.tokens.push(Token::Text(text));
        }
        self.pos = end;
    }

    fn matches_ignore_case(&self, offset: usize, needle: &str) -> bool {
        for (k, nc) in needle.chars().enumerate() {
            match self.chars.get(self.pos + offset + k) {
                Some(c) if c.eq_ignore_ascii_case(&nc) => {}
                _ => return false,
            }
        }
        true
    }

    fn find_sequence(&self, from: usize, needle: &[char]) -> Option<usize> {
        if needle.is_empty() || self.chars.len() < needle.len() {
            return None;
        }
        let mut i = from;
        while i + needle.len() <= self.chars.len() {
            if self.chars[i..i + needle.len()] == *needle {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn find_end_tag(&self, tag: &str) -> Option<usize> {
        let tag_len = tag.chars().count();
        let mut i = self.pos;
        while i + 2 + tag_len <= self.chars.len() {
            if self.chars[i] == '<' && self.chars[i + 1] == '/' {
                let name_matches = tag
                    .chars()
                    .enumerate()
                    .all(|(k, tc)| self.chars[i + 2 + k].eq_ignore_ascii_case(&tc));
                if name_matches {
                    let after = self.chars.get(i + 2 + tag_len).copied();
                    let terminated = match after {
                        Some(c) => c.is_ascii_whitespace() || c == '/' || c == '>',
                        None => true,
                    };
                    if terminated {
                        return Some(i);
                    }
                }
            }
            i += 1;
        }
        None
    }
}

fn is_rawtext_element(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes"
    )
}

fn is_rcdata_element(name: &str) -> bool {
    matches!(name, "title" | "textarea")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(input: &str) -> Vec<Token> {
        tokenize(input)
    }

    #[test]
    fn tokenizes_simple_element_with_text() {
        let toks = tags("<p>Hello</p>");
        assert_eq!(
            toks,
            vec![
                Token::StartTag {
                    name: "p".into(),
                    attributes: vec![],
                    self_closing: false
                },
                Token::Text("Hello".into()),
                Token::EndTag { name: "p".into() },
                Token::Eof,
            ]
        );
    }

    #[test]
    fn parses_attributes_quoted_and_unquoted() {
        let toks = tags(r#"<a href="/x" data-n=3 disabled>"#);
        let Token::StartTag {
            name, attributes, ..
        } = &toks[0]
        else {
            panic!("expected start tag, got {:?}", toks[0]);
        };
        assert_eq!(name, "a");
        assert_eq!(
            attributes[0],
            Attribute {
                name: "href".into(),
                value: "/x".into()
            }
        );
        assert_eq!(
            attributes[1],
            Attribute {
                name: "data-n".into(),
                value: "3".into()
            }
        );
        assert_eq!(
            attributes[2],
            Attribute {
                name: "disabled".into(),
                value: String::new()
            }
        );
    }

    #[test]
    fn decodes_entities_in_text_and_attributes() {
        let toks = tags(r#"<a title="Tom &amp; Jerry">a &lt; b</a>"#);
        let Token::StartTag { attributes, .. } = &toks[0] else {
            panic!()
        };
        assert_eq!(attributes[0].value, "Tom & Jerry");
        assert_eq!(toks[1], Token::Text("a < b".into()));
    }

    #[test]
    fn self_closing_tag() {
        let toks = tags("<br/>");
        assert_eq!(
            toks[0],
            Token::StartTag {
                name: "br".into(),
                attributes: vec![],
                self_closing: true
            }
        );
    }

    #[test]
    fn script_is_rawtext_not_parsed_as_markup() {
        let toks = tags("<script>if (a<b && c>d) {}</script>");
        assert_eq!(toks[0].tag_name(), Some("script"));
        assert_eq!(toks[1], Token::Text("if (a<b && c>d) {}".into()));
        assert_eq!(
            toks[2],
            Token::EndTag {
                name: "script".into()
            }
        );
    }

    #[test]
    fn rcdata_decodes_but_does_not_parse_tags() {
        let toks = tags("<title>A &amp; B <not a tag></title>");
        assert_eq!(toks[1], Token::Text("A & B <not a tag>".into()));
    }

    #[test]
    fn comment_and_doctype() {
        let toks = tags("<!DOCTYPE html><!-- hi --><p>");
        assert_eq!(
            toks[0],
            Token::Doctype {
                name: Some("html".into()),
                force_quirks: false
            }
        );
        assert_eq!(toks[1], Token::Comment(" hi ".into()));
        assert_eq!(toks[2].tag_name(), Some("p"));
    }

    #[test]
    fn stray_lt_is_text() {
        let toks = tags("a < b");
        assert_eq!(toks[0], Token::Text("a < b".into()));
    }

    #[test]
    fn unterminated_tag_runs_to_eof_without_panicking() {
        let toks = tags("<div class=\"x");
        assert_eq!(toks[0].tag_name(), Some("div"));
        assert_eq!(toks.last(), Some(&Token::Eof));
    }
}
