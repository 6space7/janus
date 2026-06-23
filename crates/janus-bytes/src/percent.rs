//! Percent-encoding and the WHATWG percent-encode sets.
//!
//! Only the bytes in a component's encode set are escaped; everything else —
//! including any existing `%XX` — is passed through, so re-encoding an
//! already-parsed URL is idempotent (we never double-encode `%`).

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The C0-control-or-non-ASCII baseline shared by every encode set.
#[inline]
const fn c0_control_or_high(b: u8) -> bool {
    b <= 0x1f || b > 0x7e
}

/// The fragment percent-encode set.
#[inline]
pub(crate) fn fragment_set(b: u8) -> bool {
    c0_control_or_high(b) || matches!(b, b' ' | b'"' | b'<' | b'>' | b'`')
}

/// The query percent-encode set.
#[inline]
pub(crate) fn query_set(b: u8) -> bool {
    c0_control_or_high(b) || matches!(b, b' ' | b'"' | b'#' | b'<' | b'>')
}

/// The path percent-encode set.
#[inline]
pub(crate) fn path_set(b: u8) -> bool {
    query_set(b) || matches!(b, b'?' | b'`' | b'{' | b'}')
}

/// The userinfo percent-encode set.
#[inline]
pub(crate) fn userinfo_set(b: u8) -> bool {
    path_set(b)
        || matches!(
            b,
            b'/' | b':' | b';' | b'=' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|'
        )
}

/// Percent-encode `input`, escaping every byte for which `in_set` returns true.
pub(crate) fn percent_encode(input: &str, in_set: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if in_set(b) {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 0x0f) as usize]));
        } else {
            // Not in the set ⇒ guaranteed ASCII (every set escapes bytes > 0x7e).
            out.push(char::from(b));
        }
    }
    out
}

#[inline]
fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode every `%XX` escape in `input` into raw bytes. Invalid escapes (a `%`
/// not followed by two hex digits) are preserved verbatim.
#[must_use]
pub fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Decode `%XX` escapes and interpret the result as UTF-8, lossily.
#[must_use]
pub fn percent_decode_str(input: &str) -> String {
    String::from_utf8_lossy(&percent_decode(input)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_only_set_members() {
        assert_eq!(percent_encode("a b", path_set), "a%20b");
        assert_eq!(percent_encode("a/b", path_set), "a/b"); // '/' not in path set
        assert_eq!(percent_encode("a b", userinfo_set), "a%20b");
        assert_eq!(percent_encode("a/b", userinfo_set), "a%2Fb"); // '/' in userinfo set
    }

    #[test]
    fn does_not_double_encode_percent() {
        assert_eq!(percent_encode("%20", path_set), "%20");
    }

    #[test]
    fn decode_round_trips_ascii_and_utf8() {
        assert_eq!(percent_decode("a%20b"), b"a b");
        assert_eq!(percent_decode_str("%E2%82%AC"), "€");
        // Invalid escape preserved.
        assert_eq!(percent_decode_str("100%"), "100%");
        assert_eq!(percent_decode_str("%zz"), "%zz");
    }
}
