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
    /// `display: flex` — a (row) flex container.
    Flex,
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

/// `justify-content` — main-axis distribution in a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JustifyContent {
    /// Pack items at the start.
    Start,
    /// Center items.
    Center,
    /// Pack items at the end.
    End,
    /// First/last flush; equal gaps between.
    SpaceBetween,
    /// Equal gaps around each item.
    SpaceAround,
}

/// `align-items` — cross-axis alignment in a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignItems {
    /// Fill the cross axis (treated as `Start` until items can be resized).
    Stretch,
    /// Align to the cross start.
    Start,
    /// Center on the cross axis.
    Center,
    /// Align to the cross end.
    End,
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
    if let Some(inner) = lower
        .strip_prefix("hsla")
        .and_then(paren_inner)
        .or_else(|| lower.strip_prefix("hsl").and_then(paren_inner))
    {
        return parse_hsl(inner);
    }
    named_color(&lower)
}

/// Parse `hsl(h, s%, l%[, a])` / `hsla(…)` and the space-separated `/ alpha`
/// form, returning an RGBA color.
fn parse_hsl(inner: &str) -> Option<Color> {
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
    let s = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
    let l = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
    let a = match parts.get(3) {
        Some(av) if av.ends_with('%') => av.trim_end_matches('%').parse::<f32>().ok()? / 100.0,
        Some(av) => av.parse::<f32>().ok()?,
        None => 1.0,
    };
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    Some(Color {
        r,
        g,
        b,
        a: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
    })
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
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
        // Greys.
        "lightgray" | "lightgrey" => Color::rgb(211, 211, 211),
        "darkgray" | "darkgrey" => Color::rgb(169, 169, 169),
        "dimgray" | "dimgrey" => Color::rgb(105, 105, 105),
        "slategray" | "slategrey" => Color::rgb(112, 128, 144),
        "gainsboro" => Color::rgb(220, 220, 220),
        "whitesmoke" => Color::rgb(245, 245, 245),
        // Reds / pinks.
        "crimson" => Color::rgb(220, 20, 60),
        "darkred" => Color::rgb(139, 0, 0),
        "tomato" => Color::rgb(255, 99, 71),
        "coral" => Color::rgb(255, 127, 80),
        "salmon" => Color::rgb(250, 128, 114),
        "pink" => Color::rgb(255, 192, 203),
        "hotpink" => Color::rgb(255, 105, 180),
        "brown" => Color::rgb(165, 42, 42),
        "chocolate" => Color::rgb(210, 105, 30),
        "tan" => Color::rgb(210, 180, 140),
        "gold" => Color::rgb(255, 215, 0),
        "khaki" => Color::rgb(240, 230, 140),
        "beige" => Color::rgb(245, 245, 220),
        "ivory" => Color::rgb(255, 255, 240),
        // Purples.
        "indigo" => Color::rgb(75, 0, 130),
        "violet" => Color::rgb(238, 130, 238),
        "plum" => Color::rgb(221, 160, 221),
        "orchid" => Color::rgb(218, 112, 214),
        "lavender" => Color::rgb(230, 230, 250),
        // Greens.
        "darkgreen" => Color::rgb(0, 100, 0),
        "forestgreen" => Color::rgb(34, 139, 34),
        "seagreen" => Color::rgb(46, 139, 87),
        "limegreen" => Color::rgb(50, 205, 50),
        "turquoise" => Color::rgb(64, 224, 208),
        // Blues.
        "darkblue" => Color::rgb(0, 0, 139),
        "midnightblue" => Color::rgb(25, 25, 112),
        "royalblue" => Color::rgb(65, 105, 225),
        "steelblue" => Color::rgb(70, 130, 180),
        "dodgerblue" => Color::rgb(30, 144, 255),
        "skyblue" => Color::rgb(135, 206, 235),
        "lightblue" => Color::rgb(173, 216, 230),
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
    fn hsl_colors() {
        // hsl(0,100%,50%) is pure red.
        assert_eq!(
            parse_color("hsl(0, 100%, 50%)"),
            Some(Color::rgb(255, 0, 0))
        );
        // hsl(120,100%,50%) is pure green; 240 is blue.
        assert_eq!(
            parse_color("hsl(120, 100%, 50%)"),
            Some(Color::rgb(0, 255, 0))
        );
        assert_eq!(
            parse_color("hsl(240 100% 50%)"),
            Some(Color::rgb(0, 0, 255))
        );
        // 0% saturation → grey at the given lightness.
        assert_eq!(
            parse_color("hsl(0, 0%, 50%)"),
            Some(Color::rgb(128, 128, 128))
        );
        // alpha via the slash form.
        assert_eq!(
            parse_color("hsl(0 100% 50% / 0.5)"),
            Some(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 128
            })
        );
    }

    #[test]
    fn extended_named_colors() {
        assert_eq!(parse_color("gold"), Some(Color::rgb(255, 215, 0)));
        assert_eq!(parse_color("LightGray"), Some(Color::rgb(211, 211, 211)));
        assert_eq!(parse_color("steelblue"), Some(Color::rgb(70, 130, 180)));
        assert_eq!(parse_color("crimson"), Some(Color::rgb(220, 20, 60)));
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
