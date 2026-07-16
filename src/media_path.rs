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
}
