//! The DOM node store — Janus's central tree.
//!
//! This is the engine's *internal* tree: `janus-html` fills it, `janus-style`
//! annotates it with computed values, `janus-layout` reads it to produce
//! geometry, and both painters consume that geometry. It is intentionally
//! distinct from the LLM-facing model (`janus-sem`), which is a *projection* of
//! this tree, not this tree itself.
//!
//! Nodes live in a [generational arena](janus_arena::Arena) (cache-friendly,
//! use-after-free-safe handles) and every name — tag names, attribute names —
//! is interned through the document's [`Interner`] so comparisons are `O(1)`.

use janus_arena::Arena;
use janus_atom::Interner;

pub use janus_atom::Atom;

/// A stable handle to a node: a [generational arena](janus_arena) index that
/// survives unrelated mutations and is rejected once its node is removed.
pub type NodeId = janus_arena::Index;

/// An element's interned name plus its attributes (interned names, raw values).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Element {
    /// The tag name, interned (already ASCII-lowercased).
    pub name: Atom,
    /// Attributes in source order: `(interned name, value)`.
    pub attributes: Vec<(Atom, String)>,
}

/// The payload of a [`Node`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NodeData {
    /// The document root.
    Document,
    /// A `<!DOCTYPE …>` node.
    Doctype {
        /// The doctype name (e.g. `html`).
        name: String,
    },
    /// An element.
    Element(Element),
    /// A run of text.
    Text(String),
    /// A comment.
    Comment(String),
}

/// A node in the tree: its data plus parent/child links (by [`NodeId`]).
#[derive(Clone, Debug)]
pub struct Node {
    /// The node's payload.
    pub data: NodeData,
    /// The parent node, or `None` for the document root.
    pub parent: Option<NodeId>,
    /// Children, in document order.
    pub children: Vec<NodeId>,
}

/// A parsed document: a node arena, the shared [`Interner`], and the root.
#[derive(Debug)]
pub struct Dom {
    nodes: Arena<Node>,
    interner: Interner,
    document: NodeId,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    /// Create an empty document containing only its root [`NodeData::Document`].
    #[must_use]
    pub fn new() -> Self {
        let mut nodes = Arena::new();
        let document = nodes.insert(Node {
            data: NodeData::Document,
            parent: None,
            children: Vec::new(),
        });
        Self {
            nodes,
            interner: Interner::new(),
            document,
        }
    }

    /// The document root node.
    #[must_use]
    pub fn document(&self) -> NodeId {
        self.document
    }

    /// The document's string interner.
    #[must_use]
    pub fn interner(&self) -> &Interner {
        &self.interner
    }

    /// Borrow a node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Mutably borrow a node.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id)
    }

    /// A node's children (empty slice if the id is stale).
    #[must_use]
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |n| n.children.as_slice())
    }

    /// A node's parent.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id).and_then(|n| n.parent)
    }

    /// Resolve an interned [`Atom`] to its string.
    #[must_use]
    pub fn resolve(&self, atom: Atom) -> &str {
        self.interner.resolve(atom)
    }

    fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.insert(Node {
            data,
            parent: None,
            children: Vec::new(),
        })
    }

    /// Create a detached element node, interning its (lowercased) names.
    pub fn create_element(&mut self, name: &str, attributes: &[(String, String)]) -> NodeId {
        let name_atom = self.interner.intern(&name.to_ascii_lowercase());
        let attributes = attributes
            .iter()
            .map(|(k, v)| (self.interner.intern(&k.to_ascii_lowercase()), v.clone()))
            .collect();
        self.create(NodeData::Element(Element {
            name: name_atom,
            attributes,
        }))
    }

    /// Create a detached text node.
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.create(NodeData::Text(text.into()))
    }

    /// Create a detached comment node.
    pub fn create_comment(&mut self, text: impl Into<String>) -> NodeId {
        self.create(NodeData::Comment(text.into()))
    }

    /// Create a detached doctype node.
    pub fn create_doctype(&mut self, name: impl Into<String>) -> NodeId {
        self.create(NodeData::Doctype { name: name.into() })
    }

    /// Append `child` to `parent`, detaching it from any previous parent first.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        if let Some(old_parent) = self.nodes.get(child).and_then(|n| n.parent) {
            if let Some(p) = self.nodes.get_mut(old_parent) {
                p.children.retain(|&c| c != child);
            }
        }
        if let Some(c) = self.nodes.get_mut(child) {
            c.parent = Some(parent);
        }
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(child);
        }
    }

    /// The tag name of an element node, if `id` is an element.
    #[must_use]
    pub fn element_name(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => Some(self.interner.resolve(e.name)),
            _ => None,
        }
    }

    /// The value of attribute `name` on element `id`, if present.
    #[must_use]
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        let wanted = self.interner.get(&name.to_ascii_lowercase())?;
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => e
                .attributes
                .iter()
                .find(|(k, _)| *k == wanted)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    /// Serialize the tree to an indented, html5lib-flavored string for tests.
    #[must_use]
    pub fn to_test_string(&self) -> String {
        let mut out = String::new();
        self.write_node(self.document, 0, &mut out);
        out
    }

    fn write_node(&self, id: NodeId, depth: usize, out: &mut String) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        let indent = "  ".repeat(depth);
        match &node.data {
            NodeData::Document => out.push_str("#document\n"),
            NodeData::Doctype { name } => {
                out.push_str(&indent);
                out.push_str("<!DOCTYPE ");
                out.push_str(name);
                out.push_str(">\n");
            }
            NodeData::Element(e) => {
                out.push_str(&indent);
                out.push('<');
                out.push_str(self.interner.resolve(e.name));
                for (k, v) in &e.attributes {
                    out.push(' ');
                    out.push_str(self.interner.resolve(*k));
                    out.push_str("=\"");
                    out.push_str(v);
                    out.push('"');
                }
                out.push_str(">\n");
            }
            NodeData::Text(t) => {
                out.push_str(&indent);
                out.push('"');
                out.push_str(t);
                out.push_str("\"\n");
            }
            NodeData::Comment(c) => {
                out.push_str(&indent);
                out.push_str("<!-- ");
                out.push_str(c);
                out.push_str(" -->\n");
            }
        }
        for &child in &node.children {
            self.write_node(child, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_walks_a_small_tree() {
        let mut dom = Dom::new();
        let html = dom.create_element("HTML", &[]);
        dom.append_child(dom.document(), html);
        let p = dom.create_element("p", &[("class".into(), "lead".into())]);
        dom.append_child(html, p);
        let text = dom.create_text("hi");
        dom.append_child(p, text);

        assert_eq!(dom.element_name(html), Some("html")); // name lowercased
        assert_eq!(dom.attr(p, "CLASS"), Some("lead")); // attr lookup case-insensitive
        assert_eq!(dom.children(html), &[p]);
        assert_eq!(dom.parent(p), Some(html));
        assert_eq!(
            dom.to_test_string(),
            "#document\n  <html>\n    <p class=\"lead\">\n      \"hi\"\n"
        );
    }

    #[test]
    fn append_reparents() {
        let mut dom = Dom::new();
        let a = dom.create_element("a", &[]);
        let b = dom.create_element("b", &[]);
        let t = dom.create_text("x");
        dom.append_child(dom.document(), a);
        dom.append_child(dom.document(), b);
        dom.append_child(a, t);
        dom.append_child(b, t); // move t from a to b
        assert_eq!(dom.children(a), &[] as &[NodeId]);
        assert_eq!(dom.children(b), &[t]);
        assert_eq!(dom.parent(t), Some(b));
    }
}
