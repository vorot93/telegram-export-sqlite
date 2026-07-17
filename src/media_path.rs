//! Shared validation for relative media paths and href schemes used by both
//! HTML export and bundle media copying.

pub fn safe_media_path(raw: &str) -> Option<String> {
    if raw
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    if raw.trim() != raw
        || raw.is_empty()
        || raw.starts_with('/')
        || raw.contains('\\')
        || href_scheme(raw).is_some()
        || raw.split('/').any(unsafe_media_path_component)
    {
        return None;
    }

    Some(raw.to_string())
}

/// Percent-encode a validated relative media path for use in an HTML `href`/`src`.
/// Forward slashes are preserved as path separators; every other byte outside the
/// RFC 3986 unreserved set (`A-Za-z0-9-._~`) is percent-encoded, so `#`, `?`, `%`,
/// space, and non-ASCII bytes cannot truncate or break the link (C46). Over-encoding
/// path-safe punctuation is harmless: the browser decodes it back to the same file.
/// Expects an already-`safe_media_path`-validated input (no `..`, no leading `/`,
/// no backslash, no control characters).
pub fn encode_media_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for &byte in raw.as_bytes() {
        match byte {
            b'/' => out.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(hex_upper(byte >> 4));
                out.push(hex_upper(byte & 0x0F));
            }
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + nibble - 10) as char,
    }
}

fn unsafe_media_path_component(component: &str) -> bool {
    match percent_decode_ascii(component) {
        Some(decoded) => decoded == "." || decoded == "..",
        None => true,
    }
}

fn percent_decode_ascii(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            let value = (high << 4) | low;
            if value > 0x7F {
                return None;
            }
            decoded.push(value as char);
            index += 3;
        } else {
            decoded.push(bytes[index] as char);
            index += 1;
        }
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn href_scheme(raw: &str) -> Option<String> {
    let colon = raw.find(':')?;
    let delimiter = raw.find(['/', '?', '#']).unwrap_or(raw.len());
    if colon > delimiter {
        return None;
    }

    let scheme = &raw[..colon];
    if is_valid_scheme(scheme) {
        Some(scheme.to_ascii_lowercase())
    } else {
        None
    }
}

/// Validate a link href for output. Returns the href unchanged when safe, or
/// `None` when it should be dropped (keeping only the link's visible text).
/// Rejects control characters, leading/trailing whitespace, and any scheme
/// outside a conservative allowlist (`http`, `https`, `mailto`, `tel`, `tg`);
/// relative and `#`-anchor hrefs are allowed. Shared by HTML export and LLM
/// export so both apply the same policy.
pub fn safe_href(raw: &str) -> Option<String> {
    if raw
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    if raw.trim() != raw {
        return None;
    }
    // Reject protocol-relative ("//host") and UNC/backslash variants that browsers
    // normalize to an authority: they navigate off-origin despite the scheme allowlist.
    if raw.replace('\\', "/").starts_with("//") {
        return None;
    }
    if raw.is_empty() || raw.starts_with('#') {
        return Some(raw.to_string());
    }

    if let Some(scheme) = href_scheme(raw) {
        return matches!(scheme.as_str(), "http" | "https" | "mailto" | "tel" | "tg")
            .then(|| raw.to_string());
    }

    Some(raw.to_string())
}

fn is_valid_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_relative_media_path() {
        assert_eq!(
            safe_media_path("photos/photo_1.jpg").as_deref(),
            Some("photos/photo_1.jpg")
        );
    }

    #[test]
    fn encode_media_path_leaves_unreserved_characters_untouched() {
        assert_eq!(
            encode_media_path("photos/photo_1.jpg"),
            "photos/photo_1.jpg"
        );
        assert_eq!(encode_media_path("a-b_c.d~e/f"), "a-b_c.d~e/f");
    }

    #[test]
    fn encode_media_path_percent_encodes_unsafe_href_characters() {
        // C46: an unencoded '#', '?', '%', or space silently truncates or breaks an
        // href/src, so a real filename containing them points nowhere.
        assert_eq!(encode_media_path("dir/my file.jpg"), "dir/my%20file.jpg");
        assert_eq!(encode_media_path("dir/a#b.jpg"), "dir/a%23b.jpg");
        assert_eq!(encode_media_path("dir/a?b.jpg"), "dir/a%3Fb.jpg");
        assert_eq!(encode_media_path("dir/100%.png"), "dir/100%25.png");
    }

    #[test]
    fn encode_media_path_preserves_slashes_and_encodes_utf8_by_byte() {
        // '/' stays a separator; multibyte UTF-8 is encoded byte-by-byte so the
        // browser decodes it back to the same on-disk filename.
        assert_eq!(encode_media_path("a/b/c.jpg"), "a/b/c.jpg");
        assert_eq!(encode_media_path("caf\u{e9}.jpg"), "caf%C3%A9.jpg");
    }

    #[test]
    fn rejects_absolute_and_traversal_paths() {
        assert_eq!(safe_media_path("/etc/passwd"), None);
        assert_eq!(safe_media_path("../secret.jpg"), None);
        assert_eq!(safe_media_path("a/%2e%2e/b.jpg"), None);
        assert_eq!(safe_media_path("a\\b.jpg"), None);
        assert_eq!(safe_media_path("http://example.com/x.jpg"), None);
    }

    #[test]
    fn safe_href_allows_web_schemes_and_drops_dangerous_ones() {
        for allowed in [
            "https://e.com",
            "http://e.com",
            "mailto:a@b.com",
            "tel:+1555",
            "tg://resolve?domain=x",
            "/relative/path",
            "#anchor",
        ] {
            assert_eq!(safe_href(allowed).as_deref(), Some(allowed));
        }
        assert_eq!(safe_href("javascript:alert(1)"), None);
        assert_eq!(safe_href("data:text/html,<script>"), None);
        assert_eq!(safe_href("file:///etc/passwd"), None);
        assert_eq!(safe_href(" https://e.com "), None); // leading/trailing whitespace
    }

    #[test]
    fn safe_href_rejects_protocol_relative_and_unc_references() {
        // Scheme-less "//host" (and backslash variants browsers normalize) navigate
        // off-origin despite the allowlist; a single leading slash stays allowed.
        assert_eq!(safe_href("//evil.example/x"), None);
        assert_eq!(safe_href("\\\\evil.example\\x"), None);
        assert_eq!(safe_href("/\\evil.example"), None);
        assert_eq!(
            safe_href("/relative/path").as_deref(),
            Some("/relative/path")
        );
    }
}
