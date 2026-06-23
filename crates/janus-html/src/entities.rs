//! Character-reference decoding.
//!
//! Supports numeric references (`&#169;`, `&#xA9;`) and a curated set of the
//! most common named references. The full WHATWG named-entity table (~2200
//! entries) is deferred; unknown references are left verbatim (including the
//! leading `&`), which is the safe, lossless behavior.

/// Decode all character references in `input`. Allocation-free fast path when
/// the input contains no `&`.
#[must_use]
pub fn decode(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // A reference is `&` … `;` within a short window.
        if let Some(rel) = chars[i + 1..].iter().take(32).position(|&c| c == ';') {
            let semi = i + 1 + rel;
            let name: String = chars[i + 1..semi].iter().collect();
            if let Some(decoded) = decode_one(&name) {
                out.push(decoded);
                i = semi + 1;
                continue;
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

fn decode_one(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix('#') {
        let code = if let Some(hex) = rest.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            rest.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    named(name)
}

fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "euro" => '€',
        "pound" => '£',
        "cent" => '¢',
        "yen" => '¥',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "laquo" => '«',
        "raquo" => '»',
        "ldquo" => '“',
        "rdquo" => '”',
        "lsquo" => '‘',
        "rsquo" => '’',
        "middot" => '·',
        "bull" => '•',
        "sect" => '§',
        "para" => '¶',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_and_numeric() {
        assert_eq!(decode("a &amp; b"), "a & b");
        assert_eq!(decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode("&#169;"), "©");
        assert_eq!(decode("&#xA9;"), "©");
        assert_eq!(decode("&nbsp;"), "\u{a0}");
    }

    #[test]
    fn leaves_unknown_refs_verbatim() {
        assert_eq!(decode("AT&T"), "AT&T");
        assert_eq!(decode("&notareal;"), "&notareal;");
        assert_eq!(decode("100% &"), "100% &");
    }

    #[test]
    fn fast_path_without_ampersand() {
        assert_eq!(decode("plain text"), "plain text");
    }
}
