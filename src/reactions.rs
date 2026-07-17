//! Shared reaction/emoji helpers used by the HTML and LLM renderers.
//!
//! Only these parsing helpers are shared; each renderer keeps its own
//! `render_reactions*` wrapper because HTML and Markdown format reactions
//! differently.

use serde_json::Value;

/// The display emoji for a stored reaction, mapping a few well-known named
/// reactions (`thumbs_up`, `+1`, `like`) to 👍. Returns `None` when the
/// reaction carries neither an `emoji` nor an `emoticon` field (for example a
/// custom `document_id`-only reaction).
pub fn reaction_emoji(reaction: &Value) -> Option<String> {
    reaction
        .get("emoji")
        .and_then(Value::as_str)
        .or_else(|| reaction.get("emoticon").and_then(Value::as_str))
        .map(named_emoji)
}

fn named_emoji(raw: &str) -> String {
    match raw {
        "thumbs_up" | "+1" | "like" => "👍".to_string(),
        raw => raw.to_string(),
    }
}

/// The vote count for a stored reaction: an integer `count`, a stringized
/// integer `count`, or `1` when the field is absent or unparseable.
pub fn reaction_count(reaction: &Value) -> i64 {
    reaction
        .get("count")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::{named_emoji, reaction_count, reaction_emoji};
    use serde_json::json;

    #[test]
    fn named_emoji_maps_known_aliases_and_passes_others_through() {
        assert_eq!(named_emoji("thumbs_up"), "👍");
        assert_eq!(named_emoji("+1"), "👍");
        assert_eq!(named_emoji("like"), "👍");
        assert_eq!(named_emoji("🔥"), "🔥");
        assert_eq!(named_emoji("custom_reaction"), "custom_reaction");
    }

    #[test]
    fn reaction_count_reads_int_string_or_defaults_to_one() {
        assert_eq!(reaction_count(&json!({ "count": 4 })), 4);
        assert_eq!(reaction_count(&json!({ "count": "4" })), 4);
        assert_eq!(reaction_count(&json!({})), 1);
    }

    #[test]
    fn reaction_emoji_prefers_emoji_then_emoticon_else_none() {
        assert_eq!(
            reaction_emoji(&json!({ "emoji": "❤" })).as_deref(),
            Some("❤")
        );
        assert_eq!(
            reaction_emoji(&json!({ "emoticon": "thumbs_up" })).as_deref(),
            Some("👍")
        );
        assert_eq!(reaction_emoji(&json!({ "document_id": "9" })), None);
    }
}
