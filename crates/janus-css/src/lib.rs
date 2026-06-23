//! From-scratch CSS parsing: a stylesheet/rule model plus a pragmatic parser.
//!
//! Produces a [`Stylesheet`] of [`StyleRule`]s — each a list of [`Selector`]s
//! and a list of [`Declaration`]s — for the cascade in `janus-style`. Supports
//! type, `*`, `.class`, `#id`, and compound selectors with the descendant and
//! child (`>`) combinators, `!important`, comment stripping, and the inline
//! `style="…"` form via [`parse_declarations`].
//!
//! Out of scope for now (tracked, with cssparser as the oracle): the full token
//! grammar, attribute/pseudo selectors, sibling combinators, `@media`/`@supports`
//! contents (at-rules are skipped), and escapes.

/// Selector specificity as the `(id, class, type)` triple. Ordered so that a
/// higher tuple wins the cascade.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug)]
pub struct Specificity {
    /// Number of `#id` selectors.
    pub a: u32,
    /// Number of `.class` selectors.
    pub b: u32,
    /// Number of type selectors.
    pub c: u32,
}

/// A compound selector: an optional tag plus any id/classes applied together
/// (no combinator). A missing `tag` (e.g. from `*` or a bare `.foo`) matches
/// any element.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SimpleSelector {
    /// The tag name (ASCII-lowercased), or `None` for `*`/no type constraint.
    pub tag: Option<String>,
    /// The `#id`, if any.
    pub id: Option<String>,
    /// The `.class` names.
    pub classes: Vec<String>,
    /// Attribute selectors like `[type="text"]`.
    pub attrs: Vec<AttrSelector>,
}

/// The match operator of an attribute selector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AttrOp {
    /// `[attr]` — the attribute is present.
    Exists,
    /// `[attr=v]` — exact match.
    Equals,
    /// `[attr~=v]` — whitespace-separated list contains `v`.
    Includes,
    /// `[attr^=v]` — value starts with `v`.
    Prefix,
    /// `[attr$=v]` — value ends with `v`.
    Suffix,
    /// `[attr*=v]` — value contains `v`.
    Substring,
}

/// An attribute selector, e.g. `[type="text"]` or `[disabled]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AttrSelector {
    /// Attribute name (ASCII-lowercased).
    pub name: String,
    /// The match operator.
    pub op: AttrOp,
    /// The comparison value (`None` only for [`AttrOp::Exists`]).
    pub value: Option<String>,
}

/// A full selector: a descendant chain of compounds, subject last.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Selector {
    /// Compounds left-to-right; the last is the subject, earlier ones are
    /// ancestor constraints joined by the descendant combinator.
    pub compounds: Vec<SimpleSelector>,
}

impl Selector {
    /// This selector's [`Specificity`].
    #[must_use]
    pub fn specificity(&self) -> Specificity {
        let mut s = Specificity::default();
        for compound in &self.compounds {
            if compound.id.is_some() {
                s.a += 1;
            }
            s.b += u32::try_from(compound.classes.len()).unwrap_or(u32::MAX);
            s.b += u32::try_from(compound.attrs.len()).unwrap_or(u32::MAX);
            if compound.tag.is_some() {
                s.c += 1;
            }
        }
        s
    }

    /// The subject (rightmost) compound — the element the rule targets.
    #[must_use]
    pub fn subject(&self) -> &SimpleSelector {
        self.compounds
            .last()
            .expect("selector has at least one compound")
    }
}

/// A single `name: value` declaration, with its `!important` flag.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Declaration {
    /// The property name, ASCII-lowercased.
    pub name: String,
    /// The property value (trimmed; `!important` removed).
    pub value: String,
    /// Whether the declaration carried `!important`.
    pub important: bool,
}

/// A style rule: a selector list and its declaration block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StyleRule {
    /// The comma-separated selectors this rule applies to.
    pub selectors: Vec<Selector>,
    /// The declarations in the block.
    pub declarations: Vec<Declaration>,
}

/// A parsed stylesheet.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Stylesheet {
    /// The style rules, in source order.
    pub rules: Vec<StyleRule>,
}

impl Stylesheet {
    /// Parse a stylesheet from CSS source.
    #[must_use]
    pub fn parse(css: &str) -> Stylesheet {
        Stylesheet {
            rules: parse_rules(css),
        }
    }
}

/// Parse a declaration block body (no surrounding braces) — also the parser for
/// an inline `style="…"` attribute.
#[must_use]
pub fn parse_declarations(input: &str) -> Vec<Declaration> {
    let mut declarations = Vec::new();
    for chunk in input.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let Some((name, value)) = chunk.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let mut value = value.trim().to_string();
        let mut important = false;
        if let Some(idx) = value.to_ascii_lowercase().rfind("!important") {
            important = true;
            value = value[..idx].trim_end().to_string();
        }
        if name.is_empty() || value.is_empty() {
            continue;
        }
        declarations.push(Declaration {
            name,
            value,
            important,
        });
    }
    declarations
}

fn parse_rules(css: &str) -> Vec<StyleRule> {
    let css = strip_comments(css);
    let chars: Vec<char> = css.chars().collect();
    let mut rules = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '@' {
            i = skip_at_rule(&chars, i);
            continue;
        }

        let prelude_start = i;
        while i < chars.len() && chars[i] != '{' {
            i += 1;
        }
        if i >= chars.len() {
            break; // no block — stop
        }
        let prelude: String = chars[prelude_start..i].iter().collect();
        i += 1; // consume '{'

        let block_start = i;
        let mut depth = 1;
        while i < chars.len() {
            match chars[i] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let block: String = chars[block_start..i].iter().collect();
        if i < chars.len() {
            i += 1; // consume '}'
        }

        let selectors = parse_selector_list(&prelude);
        if !selectors.is_empty() {
            rules.push(StyleRule {
                selectors,
                declarations: parse_declarations(&block),
            });
        }
    }
    rules
}

/// Skip an at-rule starting at `chars[i] == '@'`, returning the index past its
/// `;` terminator or balanced `{ … }` block.
fn skip_at_rule(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i] != ';' && chars[i] != '{' {
        i += 1;
    }
    match chars.get(i) {
        Some(';') => i + 1,
        Some('{') => {
            i += 1;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            i
        }
        _ => i,
    }
}

fn parse_selector_list(prelude: &str) -> Vec<Selector> {
    prelude
        .split(',')
        .filter_map(|s| parse_selector(s.trim()))
        .collect()
}

fn parse_selector(input: &str) -> Option<Selector> {
    // Normalize the child combinator so it tokenizes regardless of spacing.
    let normalized = input.replace('>', " > ");
    let mut compounds = Vec::new();
    for part in normalized.split_whitespace() {
        if matches!(part, ">" | "+" | "~") {
            continue; // combinators approximated as descendant for now
        }
        compounds.push(parse_compound(part)?);
    }
    if compounds.is_empty() {
        None
    } else {
        Some(Selector { compounds })
    }
}

fn parse_compound(part: &str) -> Option<SimpleSelector> {
    let chars: Vec<char> = part.chars().collect();
    let mut selector = SimpleSelector::default();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => i += 1,
            '.' => {
                i += 1;
                let name = read_ident(&chars, &mut i);
                if name.is_empty() {
                    return None;
                }
                selector.classes.push(name);
            }
            '#' => {
                i += 1;
                let name = read_ident(&chars, &mut i);
                if name.is_empty() {
                    return None;
                }
                selector.id = Some(name);
            }
            '[' => {
                let attr = parse_attr(&chars, &mut i)?;
                selector.attrs.push(attr);
            }
            c if is_ident_start(c) => {
                let name = read_ident(&chars, &mut i);
                selector.tag = Some(name.to_ascii_lowercase());
            }
            // Pseudo selectors and the like are unsupported: drop the selector.
            _ => return None,
        }
    }
    Some(selector)
}

/// Parse a `[…]` attribute selector starting at `chars[*i] == '['`.
fn parse_attr(chars: &[char], i: &mut usize) -> Option<AttrSelector> {
    *i += 1; // past '['
    skip_ws(chars, i);
    let name = read_ident(chars, i).to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    skip_ws(chars, i);
    let op = match chars.get(*i) {
        Some(']') => {
            *i += 1;
            return Some(AttrSelector {
                name,
                op: AttrOp::Exists,
                value: None,
            });
        }
        Some('=') => {
            *i += 1;
            AttrOp::Equals
        }
        Some('~') if chars.get(*i + 1) == Some(&'=') => {
            *i += 2;
            AttrOp::Includes
        }
        Some('^') if chars.get(*i + 1) == Some(&'=') => {
            *i += 2;
            AttrOp::Prefix
        }
        Some('$') if chars.get(*i + 1) == Some(&'=') => {
            *i += 2;
            AttrOp::Suffix
        }
        Some('*') if chars.get(*i + 1) == Some(&'=') => {
            *i += 2;
            AttrOp::Substring
        }
        _ => return None,
    };
    skip_ws(chars, i);
    let value = read_attr_value(chars, i);
    skip_ws(chars, i);
    if chars.get(*i) == Some(&']') {
        *i += 1;
        Some(AttrSelector {
            name,
            op,
            value: Some(value),
        })
    } else {
        None
    }
}

fn read_attr_value(chars: &[char], i: &mut usize) -> String {
    match chars.get(*i).copied() {
        Some(quote) if quote == '"' || quote == '\'' => {
            *i += 1;
            let start = *i;
            while *i < chars.len() && chars[*i] != quote {
                *i += 1;
            }
            let value: String = chars[start..*i].iter().collect();
            if chars.get(*i) == Some(&quote) {
                *i += 1;
            }
            value
        }
        _ => {
            let start = *i;
            while *i < chars.len() && !chars[*i].is_ascii_whitespace() && chars[*i] != ']' {
                *i += 1;
            }
            chars[start..*i].iter().collect()
        }
    }
}

fn skip_ws(chars: &[char], i: &mut usize) {
    while *i < chars.len() && chars[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn read_ident(chars: &[char], i: &mut usize) -> String {
    let start = *i;
    while *i < chars.len() && is_ident_char(chars[*i]) {
        *i += 1;
    }
    chars[start..*i].iter().collect()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '-' || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn strip_comments(css: &str) -> String {
    let chars: Vec<char> = css.chars().collect();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2; // skip the closing */ (saturates harmlessly at EOF)
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_selectors_and_declarations() {
        let sheet =
            Stylesheet::parse("/* c */ h1, .lead p#x { color: red; margin: 0 auto !important; }");
        assert_eq!(sheet.rules.len(), 1);
        let rule = &sheet.rules[0];
        assert_eq!(rule.selectors.len(), 2);

        // h1
        assert_eq!(rule.selectors[0].compounds.len(), 1);
        assert_eq!(rule.selectors[0].subject().tag.as_deref(), Some("h1"));

        // .lead p#x  → descendant chain of two compounds
        let s = &rule.selectors[1];
        assert_eq!(s.compounds.len(), 2);
        assert_eq!(s.compounds[0].classes, vec!["lead".to_string()]);
        let subject = s.subject();
        assert_eq!(subject.tag.as_deref(), Some("p"));
        assert_eq!(subject.id.as_deref(), Some("x"));

        assert_eq!(
            rule.declarations[0],
            Declaration {
                name: "color".into(),
                value: "red".into(),
                important: false
            }
        );
        assert_eq!(
            rule.declarations[1],
            Declaration {
                name: "margin".into(),
                value: "0 auto".into(),
                important: true
            }
        );
    }

    #[test]
    fn specificity_ordering() {
        let id = &Stylesheet::parse("#x {a:b}").rules[0].selectors[0];
        let class = &Stylesheet::parse(".c {a:b}").rules[0].selectors[0];
        let tag = &Stylesheet::parse("div {a:b}").rules[0].selectors[0];
        assert_eq!(id.specificity(), Specificity { a: 1, b: 0, c: 0 });
        assert_eq!(class.specificity(), Specificity { a: 0, b: 1, c: 0 });
        assert_eq!(tag.specificity(), Specificity { a: 0, b: 0, c: 1 });
        assert!(id.specificity() > class.specificity());
        assert!(class.specificity() > tag.specificity());
    }

    #[test]
    fn child_combinator_tokenizes_without_spaces() {
        let s = &Stylesheet::parse("ul>li{a:b}").rules[0].selectors[0];
        assert_eq!(s.compounds.len(), 2);
        assert_eq!(s.compounds[0].tag.as_deref(), Some("ul"));
        assert_eq!(s.subject().tag.as_deref(), Some("li"));
    }

    #[test]
    fn attribute_selectors_parse() {
        let s = &Stylesheet::parse("input[type=\"text\"][disabled] { a: b }").rules[0].selectors[0];
        let subject = s.subject();
        assert_eq!(subject.tag.as_deref(), Some("input"));
        assert_eq!(subject.attrs.len(), 2);
        assert_eq!(
            subject.attrs[0],
            AttrSelector {
                name: "type".into(),
                op: AttrOp::Equals,
                value: Some("text".into())
            }
        );
        assert_eq!(
            subject.attrs[1],
            AttrSelector {
                name: "disabled".into(),
                op: AttrOp::Exists,
                value: None
            }
        );
        // Each attr selector adds class-level specificity.
        assert_eq!(s.specificity(), Specificity { a: 0, b: 2, c: 1 });

        let sub = &Stylesheet::parse("a[href*=\"github\"]{a:b}").rules[0].selectors[0];
        assert_eq!(sub.subject().attrs[0].op, AttrOp::Substring);
    }

    #[test]
    fn at_rules_are_skipped() {
        let sheet = Stylesheet::parse("@import url(x); @media screen { p { a: b } } div { c: d }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].selectors[0].subject().tag.as_deref(),
            Some("div")
        );
    }

    #[test]
    fn inline_declarations() {
        let decls = parse_declarations("color: blue; font-weight: bold");
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "color");
        assert_eq!(decls[1].value, "bold");
    }

    #[test]
    fn unsupported_selectors_are_dropped() {
        // a:hover and [attr] are not supported → those selectors vanish.
        let sheet = Stylesheet::parse("a:hover { x: y } div { x: y }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].selectors[0].subject().tag.as_deref(),
            Some("div")
        );
    }
}
