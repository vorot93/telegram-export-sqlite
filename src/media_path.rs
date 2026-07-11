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
}
