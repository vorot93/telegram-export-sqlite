use crate::export_rows::{
    AttachmentRow, ExportRows, MessageRow, PollOptionRow, PollRow, ServiceEventRow,
};
use crate::media_path::safe_href;
use crate::model::{TextEntity, TextEntityKind};
use crate::time::parse_utc;
use chrono::{Datelike, NaiveDate, Timelike};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Render a message's stored Telegram rich-text entities to compact Markdown.
/// Falls back to `plain_text` when the entity JSON is empty or unparseable.
/// Newlines inside the message are preserved (stored as `\n` `Text` entities).
fn render_message_text(text_entities_json: &str, plain_text: Option<&str>) -> String {
    let entities: Vec<TextEntity> = match serde_json::from_str(text_entities_json) {
        Ok(entities) => entities,
        Err(_) => return plain_text.unwrap_or_default().to_string(),
    };
    if entities.is_empty() {
        return plain_text.unwrap_or_default().to_string();
    }
    render_entities(&entities)
}

fn render_entities(entities: &[TextEntity]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < entities.len() {
        let text = &entities[i].text;
        // Group a maximal run of consecutive entities that share identical text.
        let mut j = i + 1;
        while j < entities.len() && &entities[j].text == text {
            j += 1;
        }
        let run = &entities[i..j];
        // Nested formatting emits one entity per active mark for the SAME text
        // (all non-plain). Collapse those into a single wrapped run. Genuine
        // repeated plain text (e.g. "x" then "x") stays separate.
        let is_nesting_artifact =
            run.len() >= 2 && run.iter().all(|e| e.kind != TextEntityKind::Text);
        if is_nesting_artifact {
            out.push_str(&render_run(text, run));
        } else {
            for entity in run {
                out.push_str(&render_run(text, std::slice::from_ref(entity)));
            }
        }
        i = j;
    }
    out
}

/// Wrap `text` with the union of Markdown marks present in `run`, applied in a
/// fixed innermost -> outermost precedence so output is deterministic.
fn render_run(text: &str, run: &[TextEntity]) -> String {
    // A newline entity must never be wrapped in inline marks: a `<br>` inside an
    // active mark (bold, blockquote, …) is tagged with that mark's kind, and
    // wrapping "\n" would emit malformed Markdown (e.g. `**\n**`). Pass it through.
    if text == "\n" {
        return text.to_string();
    }
    let has = |kind: TextEntityKind| run.iter().any(|e| e.kind == kind);
    if has(TextEntityKind::Pre) {
        return format!("```\n{text}\n```");
    }
    let mut s = text.to_string();
    if has(TextEntityKind::Code) {
        s = format!("`{s}`");
    }
    if has(TextEntityKind::Bold) {
        s = format!("**{s}**");
    }
    if has(TextEntityKind::Italic) {
        s = format!("_{s}_");
    }
    if has(TextEntityKind::Strike) {
        s = format!("~~{s}~~");
    }
    if has(TextEntityKind::Spoiler) {
        s = format!("||{s}||");
    }
    if has(TextEntityKind::Blockquote) {
        s = format!("> {s}");
    }
    if let Some(href) = run
        .iter()
        .find(|e| e.kind == TextEntityKind::TextUrl)
        .and_then(|e| e.extra.get("href"))
        .and_then(|value| value.as_str())
        .and_then(safe_href)
    {
        s = format!("[{s}]({href})");
    }
    s
}

fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn basename(path: &str) -> Option<String> {
    path.rsplit(['/', '\\'])
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Compact inline placeholder for one attachment. Drops path/size/mime/dims;
/// keeps duration, document filename, and audio title (signal for an LLM).
fn render_media_placeholder(att: &AttachmentRow) -> String {
    let inner = match att.attachment_kind.as_str() {
        "photo" => "photo".to_string(),
        "video_file" => match att.duration_seconds {
            Some(d) => format!("video {}", format_duration(d)),
            None => "video".to_string(),
        },
        "voice" | "voice_message" => match att.duration_seconds {
            Some(d) => format!("voice {}", format_duration(d)),
            None => "voice".to_string(),
        },
        "video_message" => match att.duration_seconds {
            Some(d) => format!("video note {}", format_duration(d)),
            None => "video note".to_string(),
        },
        "audio" => match att.title.as_deref().filter(|t| !t.is_empty()) {
            Some(title) => format!("audio: {title}"),
            None => match att.duration_seconds {
                Some(d) => format!("audio {}", format_duration(d)),
                None => "audio".to_string(),
            },
        },
        "animation" => "GIF".to_string(),
        "sticker" => match att.title.as_deref().filter(|t| !t.is_empty()) {
            Some(title) => format!("sticker {title}"),
            None => "sticker".to_string(),
        },
        "file" => {
            let name = att
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .or_else(|| att.relative_path.as_deref().and_then(basename))
                .unwrap_or_else(|| "file".to_string());
            format!("file: {name}")
        }
        other => other.to_string(),
    };
    if att.spoiler {
        format!("[spoiler {inner}]")
    } else {
        format!("[{inner}]")
    }
}

/// `render_media_placeholder` plus an inlined transcript (`… "spoken words"`)
/// when `transcripts` has a non-empty entry for this attachment's
/// `relative_path`. Whitespace (including newlines) collapses to single spaces
/// so the transcript stays on the message line; an all-whitespace transcript
/// yields the bare placeholder (no empty quotes).
fn render_media_with_transcript(
    att: &AttachmentRow,
    transcripts: &HashMap<String, String>,
) -> String {
    let placeholder = render_media_placeholder(att);
    let Some(rel) = att.relative_path.as_deref() else {
        return placeholder;
    };
    match transcripts.get(rel) {
        Some(text) => {
            // Collapse whitespace/newlines to single spaces so the transcript
            // stays on the message line, and flatten any embedded `"` to `'` so
            // the surrounding delimiter quotes stay unambiguous (e.g. a spoken
            // `she said "hi"` renders as `"she said 'hi'"`, not nested quotes).
            let one_line = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace('"', "'");
            if one_line.is_empty() {
                placeholder
            } else {
                format!("{placeholder} \"{one_line}\"")
            }
        }
        None => placeholder,
    }
}

fn render_reactions_md(reactions_json: &str) -> String {
    let Ok(Value::Array(reactions)) = serde_json::from_str::<Value>(reactions_json) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for reaction in &reactions {
        if let Some(emoji) = reaction_emoji(reaction) {
            parts.push(format!("{emoji}{}", reaction_count(reaction)));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("+{}", parts.join(" "))
    }
}

fn reaction_emoji(reaction: &Value) -> Option<String> {
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

fn reaction_count(reaction: &Value) -> i64 {
    reaction
        .get("count")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(1)
}

fn render_poll(poll: &PollRow, options: &[&PollOptionRow]) -> String {
    let opts: Vec<String> = options
        .iter()
        .map(|option| match option.voters {
            Some(votes) => format!("{} {votes}", option.text),
            None => option.text.clone(),
        })
        .collect();
    let closed = if poll.closed == Some(true) {
        " (closed)"
    } else {
        ""
    };
    if opts.is_empty() {
        format!("📊 {}{closed}", poll.question)
    } else {
        format!("📊 {} — {}{closed}", poll.question, opts.join(" · "))
    }
}

/// Rough token estimate: ~4 characters per token. Deliberately tokenizer-free
/// (the target model varies); the summary labels this as approximate.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub struct DocStats {
    pub participants: Vec<String>,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
}

pub fn doc_stats(rows: &ExportRows) -> DocStats {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for message in &rows.messages {
        let name = message
            .sender_name
            .clone()
            .or_else(|| message.author.clone())
            .unwrap_or_else(|| "Unknown".to_string());
        if !counts.contains_key(&name) {
            order.push(name.clone());
        }
        *counts.entry(name).or_insert(0) += 1;
    }
    // Stable sort by descending count keeps first-appearance order for ties.
    order.sort_by(|a, b| counts[b].cmp(&counts[a]));

    let dates: Vec<NaiveDate> = rows
        .timeline_items
        .iter()
        .filter_map(|item| item.timestamp.as_deref())
        .filter_map(|stamp| parse_utc(stamp).ok())
        .map(|date| date.date_naive())
        .collect();
    let fmt =
        |date: &NaiveDate| format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day());

    DocStats {
        participants: order,
        first_date: dates.first().map(fmt),
        last_date: dates.last().map(fmt),
    }
}

fn render_header(rows: &ExportRows) -> String {
    let stats = doc_stats(rows);
    let participants = if stats.participants.is_empty() {
        "no participants".to_string()
    } else if stats.participants.len() > 30 {
        format!(
            "{}, +{} more",
            stats.participants[..30].join(", "),
            stats.participants.len() - 30
        )
    } else {
        stats.participants.join(", ")
    };
    let range = match (&stats.first_date, &stats.last_date) {
        (Some(first), Some(last)) => format!("{first} → {last}"),
        _ => "no dated messages".to_string(),
    };
    let legend = "Notation — `HH:MM Name:` msg (name only when speaker changes); \
wrapped lines indented · `#n` anchor · `↳#n` reply · `[media]` · `+👍n` reactions · \
`fwd(src):` · `_(edited)_` · `—` service · `📊` poll.";
    format!(
        "# {}\n{} msgs · {} · {}\n{}\n",
        rows.chat_title,
        rows.messages.len(),
        range,
        participants,
        legend,
    )
}

const WRAP_INDENT: &str = "       "; // 7 spaces: aligns wrapped lines under the text column

pub fn render_llm(rows: &ExportRows, transcripts: &HashMap<String, String>) -> String {
    let messages: HashMap<i64, &MessageRow> = rows
        .messages
        .iter()
        .map(|m| (m.timeline_item_id, m))
        .collect();
    let service_events: HashMap<i64, &ServiceEventRow> = rows
        .service_events
        .iter()
        .map(|s| (s.timeline_item_id, s))
        .collect();
    let mut attachments: HashMap<i64, Vec<&AttachmentRow>> = HashMap::new();
    for attachment in &rows.attachments {
        attachments
            .entry(attachment.timeline_item_id)
            .or_default()
            .push(attachment);
    }
    let polls: HashMap<i64, &PollRow> =
        rows.polls.iter().map(|p| (p.timeline_item_id, p)).collect();
    let mut poll_options: HashMap<i64, Vec<&PollOptionRow>> = HashMap::new();
    for option in &rows.poll_options {
        poll_options.entry(option.poll_id).or_default().push(option);
    }

    // Anchor pre-pass: only messages that are actually replied to get a compact
    // per-document `#n`, numbered in timeline order of appearance.
    let referenced: HashSet<i64> = rows
        .messages
        .iter()
        .filter_map(|m| m.reply_to_message_id)
        .collect();
    let mut anchors: HashMap<i64, usize> = HashMap::new();
    let mut next_anchor = 1usize;
    for item in &rows.timeline_items {
        if item.item_kind != "message" {
            continue;
        }
        let Some(message) = messages.get(&item.id) else {
            continue;
        };
        if referenced.contains(&message.telegram_message_id)
            && !anchors.contains_key(&message.telegram_message_id)
        {
            anchors.insert(message.telegram_message_id, next_anchor);
            next_anchor += 1;
        }
    }

    let mut body = String::new();
    let mut current_date: Option<Option<NaiveDate>> = None;
    let mut prev_sender: Option<String> = None;

    for item in &rows.timeline_items {
        if item.item_kind == "date_separator" {
            continue;
        }
        let datetime = item.timestamp.as_deref().and_then(|s| parse_utc(s).ok());
        let date = datetime.map(|d| d.date_naive());
        if current_date != Some(date) {
            current_date = Some(date);
            prev_sender = None;
            let header = match date {
                Some(d) => format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()),
                None => "(unknown date)".to_string(),
            };
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&format!("### {header}\n"));
        }
        let time_prefix = datetime
            .map(|d| format!("{:02}:{:02} ", d.hour(), d.minute()))
            .unwrap_or_default();

        if item.item_kind == "message" {
            let Some(message) = messages.get(&item.id) else {
                continue;
            };
            let sender = message
                .sender_name
                .clone()
                .or_else(|| message.author.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            let show_name = prev_sender.as_deref() != Some(sender.as_str());

            let mut parts: Vec<String> = Vec::new();
            if let Some(reply_id) = message.reply_to_message_id {
                parts.push(match anchors.get(&reply_id) {
                    Some(n) => format!("↳#{n}"),
                    None => "↳(reply)".to_string(),
                });
            }
            if message.forwarded_from.is_some() || message.forwarded_date.is_some() {
                parts.push(match &message.forwarded_from {
                    Some(from) => format!("fwd({from}):"),
                    None => "fwd:".to_string(),
                });
            }
            if let Some(atts) = attachments.get(&item.id) {
                for &att in atts {
                    parts.push(render_media_with_transcript(att, transcripts));
                }
            }
            let text =
                render_message_text(&message.text_entities_json, message.plain_text.as_deref());
            if !text.is_empty() {
                parts.push(text);
            }
            if let Some(poll) = polls.get(&item.id) {
                let mut opts: Vec<&PollOptionRow> =
                    poll_options.get(&poll.id).cloned().unwrap_or_default();
                opts.sort_by_key(|o| o.option_index);
                parts.push(render_poll(poll, &opts));
            }

            let body_core = parts.join(" ");
            // Message-level suffixes attach to the FIRST rendered line (the anchor
            // must sit on line 1 even when the message text spans multiple lines).
            let mut suffixes = String::new();
            if let Some(bot) = &message.via_bot {
                suffixes.push_str(&format!(" via @{bot}"));
            }
            if let Some(n) = anchors.get(&message.telegram_message_id) {
                suffixes.push_str(&format!(" #{n}"));
            }
            let reactions = render_reactions_md(&message.reactions_json);
            if !reactions.is_empty() {
                suffixes.push(' ');
                suffixes.push_str(&reactions);
            }
            if message.edited_timestamp.is_some() {
                suffixes.push_str(" _(edited)_");
            }

            let head = if show_name {
                format!("{time_prefix}{sender}: ")
            } else {
                format!("{time_prefix} ")
            };
            let mut lines = body_core.split('\n');
            body.push_str(&head);
            body.push_str(lines.next().unwrap_or(""));
            body.push_str(&suffixes);
            body.push('\n');
            for line in lines {
                body.push_str(WRAP_INDENT);
                body.push_str(line);
                body.push('\n');
            }
            prev_sender = Some(sender);
        } else {
            // service_event or unsupported
            let display = service_events
                .get(&item.id)
                .map(|s| s.display_text.clone())
                .or_else(|| item.display_text.clone());
            let line = match (item.item_kind.as_str(), display) {
                ("unsupported", Some(text)) => format!("— [unsupported] {text}"),
                ("unsupported", None) => "— [unsupported]".to_string(),
                (_, Some(text)) => format!("— {text}"),
                (_, None) => "—".to_string(),
            };
            body.push_str(&format!("{time_prefix}{line}\n"));
            prev_sender = None;
        }
    }

    let mut out = render_header(rows);
    if body.trim().is_empty() {
        out.push_str("\n_(no messages)_\n");
    } else {
        out.push('\n');
        out.push_str(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_rows::AttachmentRow;
    use crate::export_rows::{PollOptionRow, PollRow};

    fn attachment(kind: &str) -> AttachmentRow {
        AttachmentRow {
            timeline_item_id: 1,
            attachment_kind: kind.to_string(),
            relative_path: None,
            thumbnail_path: None,
            mime_type: None,
            file_size: None,
            duration_seconds: None,
            title: None,
            width: None,
            height: None,
            spoiler: false,
            ttl_seconds: None,
            skip_reason: None,
            extra_json: "{}".to_string(),
        }
    }

    #[test]
    fn plain_text_passes_through() {
        let json = r#"[{"type":"plain","text":"hello world"}]"#;
        assert_eq!(render_message_text(json, None), "hello world");
    }

    #[test]
    fn bold_and_link_map_to_markdown() {
        let bold = r#"[{"type":"bold","text":"hi"}]"#;
        assert_eq!(render_message_text(bold, None), "**hi**");
        let link = r#"[{"type":"text_link","text":"here","href":"https://e.com"}]"#;
        assert_eq!(render_message_text(link, None), "[here](https://e.com)");
    }

    #[test]
    fn text_url_keeps_safe_link_but_drops_dangerous_scheme() {
        // A safe scheme stays a real link.
        let safe = r#"[{"type":"text_link","text":"here","href":"https://e.com"}]"#;
        assert_eq!(render_message_text(safe, None), "[here](https://e.com)");
        // javascript:/data: hrefs are dropped; the visible text is kept.
        let js = r#"[{"type":"text_link","text":"click","href":"javascript:alert(1)"}]"#;
        assert_eq!(render_message_text(js, None), "click");
        let data = r#"[{"type":"text_link","text":"img","href":"data:text/html,x"}]"#;
        assert_eq!(render_message_text(data, None), "img");
    }

    #[test]
    fn nested_bold_link_collapses_to_single_run() {
        let json = r#"[{"type":"bold","text":"link"},{"type":"text_link","text":"link","href":"https://e.com"}]"#;
        assert_eq!(render_message_text(json, None), "[**link**](https://e.com)");
    }

    #[test]
    fn repeated_plain_text_is_not_merged() {
        let json = r#"[{"type":"plain","text":"x"},{"type":"plain","text":"x"}]"#;
        assert_eq!(render_message_text(json, None), "xx");
    }

    #[test]
    fn newlines_are_preserved() {
        let json = r#"[{"type":"plain","text":"a"},{"type":"plain","text":"\n"},{"type":"plain","text":"b"}]"#;
        assert_eq!(render_message_text(json, None), "a\nb");
    }

    #[test]
    fn falls_back_to_plain_text_when_unparseable_or_empty() {
        assert_eq!(
            render_message_text("not json", Some("fallback")),
            "fallback"
        );
        assert_eq!(render_message_text("[]", Some("plain")), "plain");
    }

    #[test]
    fn spoiler_is_wrapped_but_content_kept() {
        let json = r#"[{"type":"spoiler","text":"secret"}]"#;
        assert_eq!(render_message_text(json, None), "||secret||");
    }

    #[test]
    fn newline_inside_active_mark_is_not_wrapped() {
        // A <br> inside <strong> tags the "\n" entity as Bold; it must pass
        // through unwrapped so each line is bolded separately (not `**\n**`).
        let bold = r#"[{"type":"bold","text":"line1"},{"type":"bold","text":"\n"},{"type":"bold","text":"line2"}]"#;
        assert_eq!(render_message_text(bold, None), "**line1**\n**line2**");
        // Blockquote across a line break: the "> " marker prefixes each line.
        let quote = r#"[{"type":"blockquote","text":"a"},{"type":"blockquote","text":"\n"},{"type":"blockquote","text":"b"}]"#;
        assert_eq!(render_message_text(quote, None), "> a\n> b");
    }

    #[test]
    fn photo_and_unknown_kind_placeholders() {
        assert_eq!(render_media_placeholder(&attachment("photo")), "[photo]");
        assert_eq!(
            render_media_placeholder(&attachment("location")),
            "[location]"
        );
    }

    #[test]
    fn video_and_voice_show_duration() {
        let mut video = attachment("video_file");
        video.duration_seconds = Some(94);
        assert_eq!(render_media_placeholder(&video), "[video 1:34]");
        let mut voice = attachment("voice");
        voice.duration_seconds = Some(12);
        assert_eq!(render_media_placeholder(&voice), "[voice 0:12]");
    }

    #[test]
    fn file_uses_title_then_basename() {
        let mut titled = attachment("file");
        titled.title = Some("report.pdf".to_string());
        assert_eq!(render_media_placeholder(&titled), "[file: report.pdf]");
        let mut pathed = attachment("file");
        pathed.relative_path = Some("chat_001/files/build.log".to_string());
        assert_eq!(render_media_placeholder(&pathed), "[file: build.log]");
    }

    #[test]
    fn animation_and_spoiler() {
        assert_eq!(render_media_placeholder(&attachment("animation")), "[GIF]");
        let mut spoiler = attachment("photo");
        spoiler.spoiler = true;
        assert_eq!(render_media_placeholder(&spoiler), "[spoiler photo]");
    }

    #[test]
    fn html_shape_reactions() {
        let json = r#"[{"emoji":"👍","count":3},{"emoji":"❤️","count":1}]"#;
        assert_eq!(render_reactions_md(json), "+👍3 ❤️1");
    }

    #[test]
    fn json_emoticon_shape_and_named_emoji() {
        let json = r#"[{"emoticon":"thumbs_up","count":2}]"#;
        assert_eq!(render_reactions_md(json), "+👍2");
    }

    #[test]
    fn count_as_string_and_default() {
        assert_eq!(
            render_reactions_md(r#"[{"emoji":"🔥","count":"4"}]"#),
            "+🔥4"
        );
        assert_eq!(render_reactions_md(r#"[{"emoji":"🔥"}]"#), "+🔥1");
    }

    #[test]
    fn custom_emoji_and_malformed_yield_empty() {
        assert_eq!(
            render_reactions_md(r#"[{"document_id":"9","count":5}]"#),
            ""
        );
        assert_eq!(render_reactions_md("nope"), "");
        assert_eq!(render_reactions_md("[]"), "");
    }

    fn poll(question: &str, closed: Option<bool>) -> PollRow {
        PollRow {
            id: 1,
            timeline_item_id: 1,
            question: question.to_string(),
            closed,
            total_voters: None,
            extra_json: "{}".to_string(),
        }
    }

    fn option(index: i64, text: &str, voters: Option<i64>) -> PollOptionRow {
        PollOptionRow {
            poll_id: 1,
            option_index: index,
            text: text.to_string(),
            voters,
            chosen: None,
            extra_json: "{}".to_string(),
        }
    }

    #[test]
    fn poll_with_votes() {
        let p = poll("Dessert?", None);
        let a = option(0, "Cake", Some(2));
        let b = option(1, "Ice cream", Some(1));
        assert_eq!(
            render_poll(&p, &[&a, &b]),
            "📊 Dessert? — Cake 2 · Ice cream 1"
        );
    }

    #[test]
    fn closed_poll_and_no_votes() {
        let p = poll("Ship today?", Some(true));
        let a = option(0, "Yes", None);
        let b = option(1, "No", None);
        assert_eq!(
            render_poll(&p, &[&a, &b]),
            "📊 Ship today? — Yes · No (closed)"
        );
    }

    use crate::export_rows::{ExportRows, MessageRow, ServiceEventRow, TimelineRow};

    fn timeline(id: i64, ordinal: i64, kind: &str, stamp: Option<&str>) -> TimelineRow {
        TimelineRow {
            id,
            ordinal,
            item_kind: kind.to_string(),
            source_anchor: None,
            telegram_message_id: None,
            timestamp: stamp.map(str::to_string),
            original_timestamp: None,
            actor_name: None,
            display_text: None,
            extra_json: "{}".to_string(),
        }
    }

    fn message(tid: i64, tmid: i64, sender: &str, text: &str) -> MessageRow {
        MessageRow {
            timeline_item_id: tid,
            telegram_message_id: tmid,
            sender_name: Some(sender.to_string()),
            sender_inferred: false,
            edited_timestamp: None,
            plain_text: Some(text.to_string()),
            text_entities_json: format!(r#"[{{"type":"plain","text":{:?}}}]"#, text),
            reply_to_message_id: None,
            reply_to_peer_id: None,
            forwarded_from: None,
            forwarded_from_id: None,
            forwarded_date: None,
            saved_from: None,
            via_bot: None,
            author: None,
            inline_bot_buttons_json: "[]".to_string(),
            reactions_json: "[]".to_string(),
            extra_json: "{}".to_string(),
        }
    }

    fn empty_rows() -> ExportRows {
        ExportRows {
            chat_title: "War Room".to_string(),
            timeline_items: Vec::new(),
            messages: Vec::new(),
            service_events: Vec::new(),
            attachments: Vec::new(),
            polls: Vec::new(),
            poll_options: Vec::new(),
        }
    }

    #[test]
    fn header_reports_title_count_range_and_participants() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![
            timeline(1, 1, "message", Some("2026-05-30T09:15:00Z")),
            timeline(2, 2, "message", Some("2026-05-31T10:00:00Z")),
        ];
        rows.messages = vec![message(1, 100, "Alice", "hi"), message(2, 101, "Bob", "yo")];
        let out = render_llm(&rows, &HashMap::new());
        assert!(out.starts_with("# War Room\n"));
        assert!(out.contains("2 msgs · 2026-05-30 → 2026-05-31 · Alice, Bob"));
        assert!(out.contains("### 2026-05-30\n"));
        assert!(out.contains("### 2026-05-31\n"));
    }

    #[test]
    fn speaker_grouping_omits_repeated_name() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![
            timeline(1, 1, "message", Some("2026-05-30T09:15:00Z")),
            timeline(2, 2, "message", Some("2026-05-30T09:16:00Z")),
        ];
        rows.messages = vec![
            message(1, 100, "Alice", "first"),
            message(2, 101, "Alice", "second"),
        ];
        let out = render_llm(&rows, &HashMap::new());
        assert!(out.contains("09:15 Alice: first\n"));
        assert!(out.contains("09:16  second\n")); // name omitted, two spaces after time
    }

    #[test]
    fn reply_gets_reference_only_anchor() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![
            timeline(1, 1, "message", Some("2026-05-30T09:15:00Z")),
            timeline(2, 2, "message", Some("2026-05-30T09:20:00Z")),
        ];
        let mut first = message(1, 500, "Alice", "question");
        first.plain_text = Some("question".to_string());
        let mut reply = message(2, 501, "Bob", "answer");
        reply.reply_to_message_id = Some(500);
        rows.messages = vec![first, reply];
        let out = render_llm(&rows, &HashMap::new());
        assert!(out.contains("09:15 Alice: question #1\n"));
        assert!(out.contains("09:20 Bob: ↳#1 answer\n"));
    }

    #[test]
    fn service_event_line_and_grouping_reset() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![
            timeline(1, 1, "message", Some("2026-05-31T09:59:00Z")),
            timeline(2, 2, "service_event", Some("2026-05-31T10:00:00Z")),
            timeline(3, 3, "message", Some("2026-05-31T10:01:00Z")),
        ];
        rows.messages = vec![
            message(1, 800, "Alice", "before"),
            message(3, 801, "Alice", "after"),
        ];
        rows.service_events = vec![ServiceEventRow {
            timeline_item_id: 2,
            event_type: "pin_message".to_string(),
            actor_name: Some("Carol".to_string()),
            target_names_json: "[]".to_string(),
            display_text: "Carol pinned a message".to_string(),
            extra_json: "{}".to_string(),
        }];
        let out = render_llm(&rows, &HashMap::new());
        assert!(out.contains("10:00 — Carol pinned a message\n"));
        // The service event resets speaker grouping: Alice's name reappears after it
        // (without the reset this line would be a nameless continuation "10:01  after").
        assert!(out.contains("10:01 Alice: after\n"));
    }

    #[test]
    fn edited_and_forward_markers() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![timeline(1, 1, "message", Some("2026-05-31T10:06:00Z"))];
        let mut m = message(1, 700, "Bob", "Build passed");
        m.forwarded_from = Some("CI Bot".to_string());
        m.edited_timestamp = Some("2026-05-31T10:07:00Z".to_string());
        rows.messages = vec![m];
        let out = render_llm(&rows, &HashMap::new());
        assert!(out.contains("10:06 Bob: fwd(CI Bot): Build passed _(edited)_\n"));
    }

    #[test]
    fn empty_chat_is_valid_document() {
        let out = render_llm(&empty_rows(), &HashMap::new());
        assert!(out.starts_with("# War Room\n"));
        assert!(out.contains("_(no messages)_"));
    }

    #[test]
    fn author_only_sender_appears_in_participants() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![timeline(1, 1, "message", Some("2026-05-30T09:00:00Z"))];
        let mut m = message(1, 900, "placeholder", "hi");
        m.sender_name = None;
        m.author = Some("ChanBot".to_string());
        rows.messages = vec![m];
        let out = render_llm(&rows, &HashMap::new());
        assert!(
            out.contains("· ChanBot\n"),
            "author fallback listed as participant"
        );
        assert!(out.contains("09:00 ChanBot: hi\n"));
    }

    #[test]
    fn anchor_on_first_line_of_multiline_message() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![
            timeline(1, 1, "message", Some("2026-05-30T09:00:00Z")),
            timeline(2, 2, "message", Some("2026-05-30T09:01:00Z")),
        ];
        // First message spans two lines and is a reply target (so it gets anchor #1).
        let mut target = message(1, 500, "Alice", "ignored");
        target.plain_text = Some("Hello\nWorld".to_string());
        target.text_entities_json =
            r#"[{"type":"plain","text":"Hello"},{"type":"plain","text":"\n"},{"type":"plain","text":"World"}]"#
                .to_string();
        let mut reply = message(2, 501, "Bob", "hi");
        reply.reply_to_message_id = Some(500);
        rows.messages = vec![target, reply];
        let out = render_llm(&rows, &HashMap::new());
        assert!(
            out.contains("09:00 Alice: Hello #1\n"),
            "anchor on first line, not after World"
        );
        assert!(out.contains("World\n"));
    }

    #[test]
    fn voice_message_and_video_message_normalize_to_readable_placeholders() {
        let mut voice = attachment("voice_message");
        voice.duration_seconds = Some(12);
        assert_eq!(render_media_placeholder(&voice), "[voice 0:12]");

        let mut round = attachment("video_message");
        round.duration_seconds = Some(8);
        assert_eq!(render_media_placeholder(&round), "[video note 0:08]");
    }

    #[test]
    fn transcript_inlines_quoted_next_to_voice_placeholder() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![timeline(1, 1, "message", Some("2026-05-30T09:00:00Z"))];
        rows.messages = vec![message(1, 100, "Alice", "")];
        let mut voice = attachment("voice");
        voice.timeline_item_id = 1;
        voice.duration_seconds = Some(12);
        voice.relative_path = Some("voice/audio_1.ogg".to_string());
        rows.attachments = vec![voice];

        let mut transcripts = HashMap::new();
        // Newlines must collapse to spaces so the message stays on one line.
        transcripts.insert(
            "voice/audio_1.ogg".to_string(),
            "so let's push it\nto Friday".to_string(),
        );

        let out = render_llm(&rows, &transcripts);
        assert!(
            out.contains("[voice 0:12] \"so let's push it to Friday\""),
            "transcript inlined and quoted; got:\n{out}"
        );
    }

    #[test]
    fn empty_transcript_leaves_bare_placeholder() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![timeline(1, 1, "message", Some("2026-05-30T09:00:00Z"))];
        rows.messages = vec![message(1, 100, "Alice", "")];
        let mut voice = attachment("voice");
        voice.timeline_item_id = 1;
        voice.duration_seconds = Some(12);
        voice.relative_path = Some("voice/audio_1.ogg".to_string());
        rows.attachments = vec![voice];

        let mut transcripts = HashMap::new();
        transcripts.insert("voice/audio_1.ogg".to_string(), "   \n  ".to_string());

        let out = render_llm(&rows, &transcripts);
        assert!(out.contains("[voice 0:12]"), "placeholder present");
        assert!(!out.contains("\"\""), "no empty quotes; got:\n{out}");
    }

    #[test]
    fn embedded_quotes_are_flattened_to_apostrophes() {
        let mut rows = empty_rows();
        rows.timeline_items = vec![timeline(1, 1, "message", Some("2026-05-30T09:00:00Z"))];
        rows.messages = vec![message(1, 100, "Alice", "")];
        let mut voice = attachment("voice");
        voice.timeline_item_id = 1;
        voice.duration_seconds = Some(12);
        voice.relative_path = Some("voice/audio_1.ogg".to_string());
        rows.attachments = vec![voice];

        let mut transcripts = HashMap::new();
        transcripts.insert(
            "voice/audio_1.ogg".to_string(),
            r#"she said "hi" then left"#.to_string(),
        );

        let out = render_llm(&rows, &transcripts);
        assert!(
            out.contains("[voice 0:12] \"she said 'hi' then left\""),
            "embedded quotes flattened to apostrophes so delimiters stay clear; got:\n{out}"
        );
    }
}
