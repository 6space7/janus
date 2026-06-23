//! Tree construction: turn the [token stream](crate::tokenize) into a
//! [`janus_dom::Dom`] tree.
//!
//! This is a *pragmatic* tree builder, not the full WHATWG insertion-mode state
//! machine. It implements the parts P0 needs and that real pages depend on:
//! implied `<html>`/`<head>`/`<body>`, routing head-only elements into `<head>`,
//! void elements, self-closing tags, the most common optional-end-tag
//! auto-closing (`<p>`, `<li>`), and end-tag stack unwinding. The adoption
//! agency algorithm, tables' full insertion modes, and foster-parenting are
//! tracked for later (with the html5ever tree-diff oracle).

use janus_dom::{Dom, NodeId};

use crate::token::Token;
use crate::tokenizer::tokenize;

/// Parse `input` into a [`Dom`] tree.
#[must_use]
pub fn parse(input: &str) -> Dom {
    let mut builder = TreeBuilder::new();
    for token in tokenize(input) {
        builder.process(token);
    }
    builder.dom
}

struct TreeBuilder {
    dom: Dom,
    open: Vec<NodeId>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
    head_closed: bool,
}

impl TreeBuilder {
    fn new() -> Self {
        Self {
            dom: Dom::new(),
            open: Vec::new(),
            html: None,
            head: None,
            body: None,
            head_closed: false,
        }
    }

    fn current(&self) -> NodeId {
        self.open
            .last()
            .copied()
            .unwrap_or_else(|| self.dom.document())
    }

    fn process(&mut self, token: Token) {
        match token {
            Token::Doctype { name, .. } => {
                let node = self.dom.create_doctype(name.unwrap_or_default());
                let doc = self.dom.document();
                self.dom.append_child(doc, node);
            }
            Token::Comment(text) => {
                let node = self.dom.create_comment(text);
                let parent = self.current();
                self.dom.append_child(parent, node);
            }
            Token::Text(text) => self.insert_text(text),
            Token::StartTag {
                name,
                attributes,
                self_closing,
            } => {
                let attrs: Vec<(String, String)> =
                    attributes.into_iter().map(|a| (a.name, a.value)).collect();
                self.start_element(&name, attrs, self_closing);
            }
            Token::EndTag { name } => self.end_element(&name),
            Token::Eof => {}
        }
    }

    fn insert_text(&mut self, text: String) {
        let whitespace_only = text.chars().all(|c| c.is_ascii_whitespace());
        if whitespace_only {
            // Drop inter-element whitespace at the document/html/head level.
            if self.keep_whitespace_here() {
                let node = self.dom.create_text(text);
                let parent = self.current();
                self.dom.append_child(parent, node);
            }
            return;
        }
        self.ensure_body_for_flow();
        let node = self.dom.create_text(text);
        let parent = self.current();
        self.dom.append_child(parent, node);
    }

    fn keep_whitespace_here(&self) -> bool {
        match self.open.last().copied() {
            None => false,
            Some(id) => Some(id) != self.html && Some(id) != self.head,
        }
    }

    fn start_element(&mut self, name: &str, attrs: Vec<(String, String)>, self_closing: bool) {
        match name {
            "html" => {
                self.ensure_html();
                return;
            }
            "head" => {
                self.ensure_html();
                if self.head.is_none() {
                    let html = self.html.expect("html ensured");
                    let head = self.insert_in(html, name, &attrs, false);
                    self.head = Some(head);
                }
                return;
            }
            "body" => {
                self.ensure_html();
                self.head_closed = true;
                if self.body.is_none() {
                    let html = self.html.expect("html ensured");
                    let body = self.insert_in(html, name, &attrs, true);
                    self.body = Some(body);
                }
                return;
            }
            _ => {}
        }

        let void = is_void(name) || self_closing;

        if is_head_only(name) && !self.head_closed && self.body.is_none() {
            let head = self.ensure_head_node();
            self.insert_in(head, name, &attrs, !void);
            return;
        }

        self.ensure_body_for_flow();
        self.maybe_autoclose(name);
        let parent = self.current();
        self.insert_in(parent, name, &attrs, !void);
    }

    fn end_element(&mut self, name: &str) {
        match name {
            "head" => self.head_closed = true,
            "body" | "html" => {}
            _ => {
                let present = self
                    .open
                    .iter()
                    .rev()
                    .any(|&id| self.dom.element_name(id) == Some(name));
                if present {
                    while let Some(id) = self.open.pop() {
                        if self.dom.element_name(id) == Some(name) {
                            break;
                        }
                    }
                }
            }
        }
    }

    fn insert_in(
        &mut self,
        parent: NodeId,
        name: &str,
        attrs: &[(String, String)],
        push: bool,
    ) -> NodeId {
        let id = self.dom.create_element(name, attrs);
        self.dom.append_child(parent, id);
        if push {
            self.open.push(id);
        }
        id
    }

    fn ensure_html(&mut self) {
        if self.html.is_none() {
            let doc = self.dom.document();
            let html = self.dom.create_element("html", &[]);
            self.dom.append_child(doc, html);
            self.open.push(html);
            self.html = Some(html);
        }
    }

    fn ensure_head_node(&mut self) -> NodeId {
        self.ensure_html();
        if let Some(head) = self.head {
            return head;
        }
        let html = self.html.expect("html ensured");
        let head = self.dom.create_element("head", &[]);
        self.dom.append_child(html, head);
        self.head = Some(head);
        head
    }

    fn ensure_body(&mut self) {
        self.ensure_html();
        self.head_closed = true;
        if self.body.is_none() {
            let html = self.html.expect("html ensured");
            let body = self.dom.create_element("body", &[]);
            self.dom.append_child(html, body);
            self.open.push(body);
            self.body = Some(body);
        }
    }

    /// Flow content at the document/`<html>` level forces a `<body>`; content
    /// already inside an element (including head-only containers) is left alone.
    fn ensure_body_for_flow(&mut self) {
        let current = self.current();
        if current == self.dom.document() || Some(current) == self.html {
            self.ensure_body();
        }
    }

    fn maybe_autoclose(&mut self, name: &str) {
        if is_block(name) && self.current_element_is("p") {
            self.open.pop();
        }
        if name == "li" && self.current_element_is("li") {
            self.open.pop();
        }
    }

    fn current_element_is(&self, tag: &str) -> bool {
        self.open
            .last()
            .is_some_and(|&id| self.dom.element_name(id) == Some(tag))
    }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_head_only(name: &str) -> bool {
    matches!(
        name,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noscript"
            | "script"
            | "style"
            | "template"
            | "title"
    )
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "div"
            | "dl"
            | "dd"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "ul"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_document_structure() {
        let dom = parse(
            "<!DOCTYPE html><html><head><title>Hi</title></head><body><p>Hello</p></body></html>",
        );
        assert_eq!(
            dom.to_test_string(),
            "#document\n  \
             <!DOCTYPE html>\n  \
             <html>\n    \
             <head>\n      \
             <title>\n        \
             \"Hi\"\n    \
             <body>\n      \
             <p>\n        \
             \"Hello\"\n"
        );
    }

    #[test]
    fn implies_html_head_body_for_bare_fragment() {
        let dom = parse("<p>x</p>");
        assert_eq!(
            dom.to_test_string(),
            "#document\n  <html>\n    <body>\n      <p>\n        \"x\"\n"
        );
    }

    #[test]
    fn auto_closes_open_paragraph() {
        let dom = parse("<p>a<p>b");
        assert_eq!(
            dom.to_test_string(),
            "#document\n  <html>\n    <body>\n      \
             <p>\n        \"a\"\n      \
             <p>\n        \"b\"\n"
        );
    }

    #[test]
    fn nested_inline_inside_block() {
        let dom = parse("<p>Hello <b>world</b></p>");
        assert_eq!(
            dom.to_test_string(),
            "#document\n  <html>\n    <body>\n      <p>\n        \
             \"Hello \"\n        <b>\n          \"world\"\n"
        );
    }

    #[test]
    fn end_tag_unwinds_to_matching_element() {
        // The stray </span> is ignored; </div> closes the div.
        let dom = parse("<div><span>x</span></div><p>y</p>");
        let out = dom.to_test_string();
        assert!(out.contains("<div>\n"));
        assert!(out.ends_with("<p>\n        \"y\"\n"));
    }
}
