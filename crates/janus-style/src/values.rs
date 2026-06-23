//! CSS value types and their parsers (colors, lengths, keyword enums).

/// A straight-alpha 8-bit RGBA color.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha (255 = opaque).
    pub a: u8,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// Opaque white.
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    /// Fully transparent.
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// An opaque color from RGB components.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
}

/// A CSS length value (specified; percentages and `em` are resolved at layout
/// time, except `font-size` which is resolved to px during the cascade).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Length {
    /// An absolute length in CSS pixels.
    Px(f32),
    /// A multiple of the element's font size.
    Em(f32),
    /// A percentage of the containing block.
    Percent(f32),
    /// The `auto` keyword.
    Auto,
}

impl Length {
    /// Resolve to pixels given the relevant `font_size` and `percent_basis`.
    /// `Auto` resolves to `0.0` (callers that care handle `Auto` separately).
    #[must_use]
    pub fn to_px(self, font_size: f32, percent_basis: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Em(v) => v * font_size,
            Length::Percent(p) => p / 100.0 * percent_basis,
            Length::Auto => 0.0,
        }
    }
}

/// The `display` value (the subset the engine lays out today).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    /// `display: inline`.
    Inline,
    /// `display: block`.
    Block,
    /// `display: inline-block`.
    InlineBlock,
    /// `display: list-item`.
    ListItem,
    /// `display: none` — the element and its subtree are not rendered.
    None,
}

/// The `text-align` value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    /// Start/left.
    Left,
    /// Center.
    Center,
    /// End/right.
    Right,
}

/// The four sides of a box (margin, padding, border widths).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Edges<T> {
    /// Top edge.
    pub top: T,
    /// Right edge.
    pub right: T,
    /// Bottom edge.
    pub bottom: T,
    /// Left edge.
    pub left: T,
}

impl<T: Copy> Edges<T> {
    /// All four edges set to `v`.
    #[must_use]
    pub fn all(v: T) -> Edges<T> {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// Parse a CSS color: named (curated set), `#rgb`/`#rrggbb`, or `rgb()/rgba()`.
#[must_use]
pub fn parse_color(input: &str) -> Option<Color> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = s.to_ascii_lowercase();
    if let Some(inner) = lower.strip_prefix("rgba").and_then(|r| paren_inner(r)) {
        return parse_rgb(inner, true);
    }
    if let Some(inner) = lower.strip_prefix("rgb").and_then(|r| paren_inner(r)) {
        return parse_rgb(inner, false);
    }
    named_color(&lower)
}

fn paren_inner(s: &str) -> Option<&str> {
    let s = s.trim_start();
    s.strip_prefix('(').and_then(|r| r.strip_suffix(')'))
}

fn parse_hex(hex: &str) -> Option<Color> {
    let parse2 = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            let mut it = hex.chars();
            let r = it.next()?;
            let g = it.next()?;
            let b = it.next()?;
            Some(Color {
                r: parse2(&format!("{r}{r}"))?,
                g: parse2(&format!("{g}{g}"))?,
                b: parse2(&format!("{b}{b}"))?,
                a: 255,
            })
        }
        6 => Some(Color {
            r: parse2(&hex[0..2])?,
            g: parse2(&hex[2..4])?,
            b: parse2(&hex[4..6])?,
            a: 255,
        }),
        _ => None,
    }
}

fn parse_rgb(inner: &str, has_alpha: bool) -> Option<Color> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let need = if has_alpha { 4 } else { 3 };
    if parts.len() != need {
        return None;
    }
    let channel = |s: &str| s.parse::<f32>().ok().map(|v| v.clamp(0.0, 255.0) as u8);
    let r = channel(parts[0])?;
    let g = channel(parts[1])?;
    let b = channel(parts[2])?;
    let a = if has_alpha {
        let af = parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0);
        (af * 255.0).round() as u8
    } else {
        255
    };
    Some(Color { r, g, b, a })
}

fn named_color(name: &str) -> Option<Color> {
    let c = match name {
        "transparent" => Color::TRANSPARENT,
        "black" => Color::rgb(0, 0, 0),
        "white" => Color::rgb(255, 255, 255),
        "red" => Color::rgb(255, 0, 0),
        "green" => Color::rgb(0, 128, 0),
        "blue" => Color::rgb(0, 0, 255),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "silver" => Color::rgb(192, 192, 192),
        "maroon" => Color::rgb(128, 0, 0),
        "navy" => Color::rgb(0, 0, 128),
        "olive" => Color::rgb(128, 128, 0),
        "purple" => Color::rgb(128, 0, 128),
        "teal" => Color::rgb(0, 128, 128),
        "aqua" | "cyan" => Color::rgb(0, 255, 255),
        "fuchsia" | "magenta" => Color::rgb(255, 0, 255),
        "lime" => Color::rgb(0, 255, 0),
        "yellow" => Color::rgb(255, 255, 0),
        "orange" => Color::rgb(255, 165, 0),
        _ => return None,
    };
    Some(c)
}

/// Parse a single CSS length / `auto`.
#[must_use]
pub fn parse_length(input: &str) -> Option<Length> {
    let s = input.trim();
    if s.eq_ignore_ascii_case("auto") {
        return Some(Length::Auto);
    }
    if let Some(num) = s.strip_suffix('%') {
        return num.trim().parse::<f32>().ok().map(Length::Percent);
    }
    if let Some(num) = s.strip_suffix("px") {
        return num.trim().parse::<f32>().ok().map(Length::Px);
    }
    if let Some(num) = s.strip_suffix("em") {
        return num.trim().parse::<f32>().ok().map(Length::Em);
    }
    // Unitless: treat 0 as 0px; other bare numbers as px (pragmatic).
    s.parse::<f32>().ok().map(Length::Px)
}

/// Parse a 1–4 value edge shorthand (e.g. `margin: 0 auto`) into [`Edges`].
#[must_use]
pub fn parse_edges(input: &str) -> Option<Edges<Length>> {
    let vals: Vec<Length> = input.split_whitespace().filter_map(parse_length).collect();
    match vals.as_slice() {
        [a] => Some(Edges::all(*a)),
        [v, h] => Some(Edges {
            top: *v,
            right: *h,
            bottom: *v,
            left: *h,
        }),
        [t, h, b] => Some(Edges {
            top: *t,
            right: *h,
            bottom: *b,
            left: *h,
        }),
        [t, r, b, l] => Some(Edges {
            top: *t,
            right: *r,
            bottom: *b,
            left: *l,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color::WHITE));
        assert_eq!(parse_color("#ff0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("rgb(0, 128, 0)"), Some(Color::rgb(0, 128, 0)));
        assert_eq!(
            parse_color("rgba(255,0,0,0.5)"),
            Some(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 128
            })
        );
        assert_eq!(parse_color("RebeccaPurple"), None); // not in the curated set
        assert_eq!(parse_color(" navy "), Some(Color::rgb(0, 0, 128)));
    }

    #[test]
    fn lengths_and_edges() {
        assert_eq!(parse_length("12px"), Some(Length::Px(12.0)));
        assert_eq!(parse_length("1.5em"), Some(Length::Em(1.5)));
        assert_eq!(parse_length("50%"), Some(Length::Percent(50.0)));
        assert_eq!(parse_length("auto"), Some(Length::Auto));
        assert_eq!(parse_length("0"), Some(Length::Px(0.0)));

        let e = parse_edges("10px 20px").unwrap();
        assert_eq!(e.top, Length::Px(10.0));
        assert_eq!(e.right, Length::Px(20.0));
        assert_eq!(e.bottom, Length::Px(10.0));
        assert_eq!(e.left, Length::Px(20.0));
    }

    #[test]
    fn length_to_px() {
        assert_eq!(Length::Em(2.0).to_px(16.0, 0.0), 32.0);
        assert_eq!(Length::Percent(50.0).to_px(16.0, 200.0), 100.0);
    }
}
