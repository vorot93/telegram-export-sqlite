use crate::{
    error::Result,
    model::*,
    service::classify_service_event,
    text::{extract_rich_text, text_from_element},
    time::{parse_duration_seconds, parse_telegram_timestamp},
};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

#[derive(Debug, Default, Clone)]
pub struct ParserState {
    previous_sender: Option<SenderContext>,
}

#[derive(Debug, Clone)]
struct SenderContext {
    name: String,
    extra_json: Value,
}

pub fn parse_export_file(
    export_root: &Path,
    absolute_path: &Path,
    relative_path: &Path,
    source_file_parse_order: usize,
    starting_ordinal: i64,
) -> Result<ParsedExport> {
    let mut state = ParserState::default();
    parse_export_file_with_state(
        export_root,
        absolute_path,
        relative_path,
        source_file_parse_order,
        starting_ordinal,
        &mut state,
    )
}

pub fn parse_export_file_with_state(
    export_root: &Path,
    absolute_path: &Path,
    relative_path: &Path,
    source_file_parse_order: usize,
    starting_ordinal: i64,
    state: &mut ParserState,
) -> Result<ParsedExport> {
    let source = std::fs::read_to_string(absolute_path)?;
    let document = Html::parse_document(&source);
    let chat_title = select_document_text(&document, "div.page_header .content div.text")
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Unknown Chat".to_string());
    let mut parsed = ParsedExport {
        chat: Some(Chat { title: chat_title }),
        source_checksum: crate::discovery::sha256_hex(source.as_bytes()),
        ..Default::default()
    };
    let message_file_parent = relative_path.parent().unwrap_or_else(|| Path::new(""));

    let mut next_ordinal = starting_ordinal;
    for element in document.select(&selector("div.history > div.message")) {
        let telegram_message_id = parse_message_id(element);

        if has_class(element, "service") {
            if service_element_is_date_separator(element, telegram_message_id) {
                continue;
            }

            parse_service_element(
                element,
                &mut parsed,
                source_file_parse_order,
                next_ordinal,
                telegram_message_id,
            );
        } else {
            parse_message_element(
                element,
                &mut parsed,
                export_root,
                message_file_parent,
                source_file_parse_order,
                next_ordinal,
                telegram_message_id,
                &mut state.previous_sender,
            );
        }
        next_ordinal += 1;
    }

    Ok(parsed)
}

fn service_element_is_date_separator(
    element: ElementRef<'_>,
    telegram_message_id: Option<i64>,
) -> bool {
    if telegram_message_id.is_some_and(|id| id < 0) {
        return true;
    }

    let body = select_element(element, "div.body");
    let display_text = body.map(text_from_element).unwrap_or_default();
    // A genuine service event (e.g. a group-title change that happens to mention a month
    // and a year) must never be discarded as a date separator just because its text trips
    // the heuristic. Real date separators carry negative ids and do not classify.
    if classify_service_event(&display_text).is_some() {
        return false;
    }
    looks_like_date_separator(display_text.as_str())
}

fn parse_service_element(
    element: ElementRef<'_>,
    parsed: &mut ParsedExport,
    source_file_parse_order: usize,
    ordinal: i64,
    telegram_message_id: Option<i64>,
) {
    let body = select_element(element, "div.body");
    let display_text = body.map(text_from_element).unwrap_or_default();
    let source_anchor = source_anchor(element);

    if let Some(classified) = classify_service_event(&display_text) {
        parsed.timeline_items.push(TimelineItem {
            id: None,
            source_file_parse_order,
            source_anchor,
            telegram_message_id,
            ordinal,
            kind: TimelineItemKind::ServiceEvent,
            timestamp: None,
            original_timestamp: None,
            actor_name: classified.actor_name.clone(),
            actor_id: None,
            display_text: Some(classified.display_text.clone()),
            extra_json: json!({
                "classes": class_attr(element),
                "service": true,
                "classification": classified.extra_json.clone(),
                "source_html": element.html(),
                "body_html": body.map(|body| body.html()),
                "body_inner_html": body.map(|body| body.inner_html()),
            }),
        });
        let mut service_event = classified.into_model(ordinal);
        add_service_source_snippets(&mut service_event.extra_json, element, body);
        parsed.service_events.push(service_event);
        return;
    }

    parsed.timeline_items.push(TimelineItem {
        id: None,
        source_file_parse_order,
        source_anchor,
        telegram_message_id,
        ordinal,
        kind: TimelineItemKind::Unsupported,
        timestamp: None,
        original_timestamp: None,
        actor_name: None,
        actor_id: None,
        display_text: non_empty(display_text.clone()),
        extra_json: json!({
            "classes": class_attr(element),
            "service": true,
            "unclassified": true,
            "source_html": element.html(),
            "body_html": body.map(|body| body.html()),
            "body_inner_html": body.map(|body| body.inner_html()),
        }),
    });
    push_warning(
        &mut parsed.warnings,
        source_file_parse_order,
        Some(ordinal),
        WarningCode::UnknownServiceEvent,
        "unknown service event",
        json!({ "display_text": display_text }),
    );
}

#[allow(clippy::too_many_arguments)]
fn parse_message_element(
    element: ElementRef<'_>,
    parsed: &mut ParsedExport,
    export_root: &Path,
    message_file_parent: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    telegram_message_id: Option<i64>,
    previous_sender: &mut Option<SenderContext>,
) {
    let body = select_element(element, "div.body").unwrap_or(element);
    let text_element = select_element(body, "div.text");
    let original_timestamp = select_attr(body, "div.date[title]", "title");
    let timestamp = parse_optional_timestamp(
        original_timestamp.as_deref(),
        &mut parsed.warnings,
        source_file_parse_order,
        ordinal,
        "message timestamp",
    );
    let (sender_name, via_bot, sender_inferred, sender_extra) = parse_sender(
        body,
        previous_sender,
        &mut parsed.warnings,
        source_file_parse_order,
        ordinal,
        has_class(element, "joined"),
    );
    let edited_timestamp =
        parse_edited_timestamp(body, &mut parsed.warnings, source_file_parse_order, ordinal);
    let rich = text_element.map(extract_rich_text);
    let plain_text = rich
        .as_ref()
        .map(|rich| rich.plain.clone())
        .filter(|text| !text.is_empty());
    let text_entities = rich.map(|rich| rich.entities).unwrap_or_default();
    let reactions = parse_reactions(body);
    let forwarded = parse_forwarded(body, &mut parsed.warnings, source_file_parse_order, ordinal);
    let display_text = plain_text.clone();

    parsed.timeline_items.push(TimelineItem {
        id: None,
        source_file_parse_order,
        source_anchor: source_anchor(element),
        telegram_message_id,
        ordinal,
        kind: TimelineItemKind::Message,
        timestamp: timestamp.clone(),
        original_timestamp: original_timestamp.clone(),
        actor_name: sender_name.clone(),
        actor_id: None,
        display_text,
        extra_json: json!({
            "classes": class_attr(element),
            "reactions": reactions.clone(),
            "source_html": element.html(),
            "body_html": body.html(),
            "body_inner_html": body.inner_html(),
        }),
    });

    parsed.messages.push(Message {
        timeline_ordinal: ordinal,
        telegram_message_id,
        sender_name,
        sender_id: None,
        sender_inferred,
        edited_timestamp,
        plain_text,
        text_entities,
        reply_to_message_id: extract_reply_id(body),
        reply_to_peer_id: None,
        forwarded_from: forwarded.from,
        forwarded_from_id: None,
        forwarded_date: forwarded.date,
        saved_from: None,
        via_bot,
        author: None,
        inline_bot_buttons: json!([]),
        // Reuse the reactions parsed above rather than walking the DOM again.
        reactions,
        extra_json: json!({
            "classes": class_attr(element),
            "source_anchor": source_anchor(element),
            "sender": sender_extra,
            "source_html": element.html(),
            "body_html": body.html(),
            "body_inner_html": body.inner_html(),
            "text_html": text_element.map(|text| text.html()),
            "text_inner_html": text_element.map(|text| text.inner_html()),
            "reply_html": select_element(body, ".reply_to").map(|reply| reply.html()),
            "forwarded_html": select_element(body, ".forwarded").map(|forwarded| forwarded.html()),
            "edited_html": select_element(body, ".edited").map(|edited| edited.html()),
            "media_wrap_html": body
                .select(&selector(".media_wrap"))
                .map(|media_wrap| media_wrap.html())
                .collect::<Vec<_>>(),
        }),
    });

    parse_media(
        body,
        &mut parsed.attachments,
        &mut parsed.warnings,
        export_root,
        message_file_parent,
        source_file_parse_order,
        ordinal,
    );
    parse_poll(body, &mut parsed.polls, &mut parsed.poll_options, ordinal);
}

fn parse_sender(
    body: ElementRef<'_>,
    previous_sender: &mut Option<SenderContext>,
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    ordinal: i64,
    is_joined: bool,
) -> (Option<String>, Option<String>, bool, Value) {
    if let Some(from_element) = direct_child_with_class(body, "from_name") {
        let raw_sender = text_from_element(from_element);
        if raw_sender.is_empty() {
            return (None, None, false, json!({}));
        }

        let (sender_name, via_bot) = split_sender_and_bot(&raw_sender);
        let sender_extra = sender_metadata(from_element);
        if let Some(sender_name) = sender_name.clone() {
            *previous_sender = Some(SenderContext {
                name: sender_name,
                extra_json: sender_extra.clone(),
            });
        }
        return (sender_name, via_bot, false, sender_extra);
    }

    if is_joined && let Some(sender_context) = previous_sender.clone() {
        push_warning(
            warnings,
            source_file_parse_order,
            Some(ordinal),
            WarningCode::InferredSender,
            "inferred sender from previous message",
            json!({ "sender_name": sender_context.name.clone() }),
        );
        return (
            Some(sender_context.name),
            None,
            true,
            inferred_sender_metadata(&sender_context.extra_json),
        );
    }

    (None, None, false, json!({}))
}

fn inferred_sender_metadata(sender_extra: &Value) -> Value {
    let mut metadata = sender_extra.as_object().cloned().unwrap_or_default();
    metadata.insert("inferred".to_string(), json!(true));
    Value::Object(metadata)
}

fn sender_metadata(from_element: ElementRef<'_>) -> Value {
    let mut metadata = Map::new();
    let anchor = if from_element.value().name() == "a" {
        Some(from_element)
    } else {
        select_element(from_element, "a")
    };

    let href = attr_from_element_or_anchor(from_element, anchor, "href");
    let data_user_id = attr_from_element_or_anchor(from_element, anchor, "data-user-id");
    let user_id = data_user_id
        .clone()
        .or_else(|| href.as_deref().and_then(telegram_user_id_from_href));

    if let Some(href) = href {
        metadata.insert("href".to_string(), json!(href));
    }
    if let Some(data_user_id) = data_user_id {
        metadata.insert("data_user_id".to_string(), json!(data_user_id));
    }
    if let Some(user_id) = user_id {
        metadata.insert("user_id".to_string(), json!(user_id));
    }

    Value::Object(metadata)
}

fn attr_from_element_or_anchor(
    element: ElementRef<'_>,
    anchor: Option<ElementRef<'_>>,
    attr: &str,
) -> Option<String> {
    element
        .value()
        .attr(attr)
        .or_else(|| anchor.and_then(|anchor| anchor.value().attr(attr)))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn telegram_user_id_from_href(href: &str) -> Option<String> {
    let value = href.strip_prefix("tg://user?id=")?;
    let user_id = value.split('&').next().unwrap_or(value);
    (!user_id.is_empty()).then(|| user_id.to_string())
}

fn parse_edited_timestamp(
    body: ElementRef<'_>,
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    ordinal: i64,
) -> Option<String> {
    let original = select_attr(body, ".edited[title]", "title")?;
    parse_edited_timestamp_title(&original).or_else(|| {
        push_warning(
            warnings,
            source_file_parse_order,
            Some(ordinal),
            WarningCode::MalformedTimestamp,
            "malformed edited timestamp",
            json!({ "timestamp": original }),
        );
        None
    })
}

fn parse_edited_timestamp_title(original: &str) -> Option<String> {
    let trimmed = original.trim();
    if let Ok(parsed) = parse_telegram_timestamp(trimmed) {
        return Some(parsed);
    }

    let stripped = strip_edited_label(trimmed);
    if stripped != trimmed {
        parse_telegram_timestamp(stripped).ok()
    } else {
        None
    }
}

fn strip_edited_label(value: &str) -> &str {
    for prefix in ["Edited:", "edited:", "Edited", "edited"] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            return stripped.trim();
        }
    }

    value
}

fn add_service_source_snippets(
    extra_json: &mut Value,
    element: ElementRef<'_>,
    body: Option<ElementRef<'_>>,
) {
    let Some(extra) = extra_json.as_object_mut() else {
        return;
    };

    extra.insert("source_html".to_string(), json!(element.html()));
    extra.insert("body_html".to_string(), json!(body.map(|body| body.html())));
    extra.insert(
        "body_inner_html".to_string(),
        json!(body.map(|body| body.inner_html())),
    );
}

fn parse_message_id(element: ElementRef<'_>) -> Option<i64> {
    element
        .value()
        .attr("id")
        .and_then(|id| id.strip_prefix("message"))
        .and_then(|id| id.parse::<i64>().ok())
}

fn looks_like_date_separator(text: &str) -> bool {
    let normalized = text.trim();
    if normalized.eq_ignore_ascii_case("today") || normalized.eq_ignore_ascii_case("yesterday") {
        return true;
    }

    let lower = normalized.to_ascii_lowercase();
    let has_month_name = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ]
    .iter()
    .any(|month| lower.contains(month));
    let has_year = normalized
        .split(|character: char| !character.is_ascii_digit())
        .any(|part| part.len() == 4 && part.parse::<i64>().is_ok());

    has_month_name && has_year
}

fn split_sender_and_bot(raw_sender: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw_sender.trim();
    if let Some((sender, bot)) = trimmed.rsplit_once(" via @") {
        let sender = sender.trim();
        let bot = format!("@{}", bot.trim());
        return (non_empty(sender.to_string()), non_empty(bot));
    }

    (non_empty(trimmed.to_string()), None)
}

fn extract_reply_id(body: ElementRef<'_>) -> Option<i64> {
    body.select(&selector(r#"a[href*="go_to_message"]"#))
        .find_map(|anchor| anchor.value().attr("href").and_then(parse_go_to_message_id))
}

fn parse_go_to_message_id(href: &str) -> Option<i64> {
    let value = href.split("go_to_message").nth(1)?;
    first_number(value)
}

#[derive(Default)]
struct Forwarded {
    from: Option<String>,
    date: Option<String>,
}

fn parse_forwarded(
    body: ElementRef<'_>,
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    ordinal: i64,
) -> Forwarded {
    let Some(from_element) = select_element(body, "div.forwarded div.from_name") else {
        return Forwarded::default();
    };

    let full_text = text_from_element(from_element);
    let date_text = select_element(from_element, ".date").map(text_from_element);
    let from = date_text
        .as_deref()
        .and_then(|date_text| full_text.strip_suffix(date_text))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| non_empty(full_text));
    let original_date = select_attr(from_element, ".date[title]", "title");
    let date = parse_optional_timestamp(
        original_date.as_deref(),
        warnings,
        source_file_parse_order,
        ordinal,
        "forwarded timestamp",
    );

    Forwarded { from, date }
}

#[allow(clippy::too_many_arguments)]
fn parse_media(
    body: ElementRef<'_>,
    attachments: &mut Vec<Attachment>,
    warnings: &mut Vec<ImportWarning>,
    export_root: &Path,
    message_file_parent: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
) {
    let supported_media_selector = selector(
        "a.photo_wrap, a.media_photo, a.media_file, a.video_file_wrap, a.media_video, a.media_audio_file, a.media_voice_message, a.sticker_wrap, a.animated_wrap",
    );
    for media_wrap in body.select(&selector("div.media_wrap")) {
        if media_wrap
            .select(&supported_media_selector)
            .next()
            .is_some()
            || select_element(media_wrap, "div.media_poll").is_some()
        {
            continue;
        }

        push_warning(
            warnings,
            source_file_parse_order,
            Some(ordinal),
            WarningCode::UnsupportedMediaShape,
            "unsupported media shape",
            json!({
                "source_html": media_wrap.html(),
                "text": text_from_element(media_wrap),
            }),
        );
        attachments.push(Attachment {
            timeline_ordinal: ordinal,
            kind: "unsupported".to_string(),
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
            skip_reason: Some("unsupported_media_shape".to_string()),
            extra_json: json!({
                "source_html": media_wrap.html(),
                "inner_html": media_wrap.inner_html(),
                "classes": class_attr(media_wrap),
                "text": text_from_element(media_wrap),
            }),
        });
    }

    for anchor in body.select(&supported_media_selector) {
        let href = anchor.value().attr("href");
        let relative_path = href.map(|href| joined_export_path(message_file_parent, href));
        if let Some(path) = relative_path.as_ref()
            && !export_root.join(path).exists()
        {
            push_warning(
                warnings,
                source_file_parse_order,
                Some(ordinal),
                WarningCode::MissingAttachment,
                "referenced attachment is missing",
                json!({ "path": path.display().to_string() }),
            );
        }

        let thumb_path = select_attr(anchor, "img", "src")
            .map(|src| joined_export_path(message_file_parent, &src));
        let status = select_text(anchor, ".status");
        let duration_seconds = select_text(anchor, ".video_duration")
            .as_deref()
            .and_then(parse_duration_text)
            .or_else(|| status.as_deref().and_then(parse_duration_text));
        let (width, height) = parse_image_dimensions(anchor);

        attachments.push(Attachment {
            timeline_ordinal: ordinal,
            kind: media_kind(anchor).to_string(),
            relative_path,
            thumbnail_path: thumb_path,
            mime_type: href.and_then(mime_from_href),
            file_size: status.as_deref().and_then(parse_file_size),
            duration_seconds,
            title: select_text(anchor, ".title"),
            width,
            height,
            spoiler: has_class(anchor, "spoiler") || select_element(anchor, ".spoiler").is_some(),
            ttl_seconds: None,
            skip_reason: None,
            extra_json: json!({
                "href": href,
                "status": status,
                "classes": class_attr(anchor),
                "source_html": anchor.html(),
                "inner_html": anchor.inner_html(),
            }),
        });
    }
}

fn parse_poll(
    body: ElementRef<'_>,
    polls: &mut Vec<Poll>,
    poll_options: &mut Vec<PollOption>,
    ordinal: i64,
) {
    for poll in body.select(&selector("div.media_poll")) {
        let question = select_text(poll, ".question").unwrap_or_default();
        let details = select_text(poll, ".details");
        let total = select_text(poll, ".total").and_then(|text| first_number(&text));
        polls.push(Poll {
            timeline_ordinal: ordinal,
            question,
            closed: details
                .as_deref()
                .map(|details| details.to_ascii_lowercase().contains("closed")),
            total_voters: total,
            extra_json: json!({ "details": details }),
        });

        for (index, answer) in poll.select(&selector(".answer")).enumerate() {
            let details = select_text(answer, ".details");
            let raw_text = text_from_element(answer);
            let text_without_details = details
                .as_deref()
                .and_then(|details| raw_text.strip_suffix(details))
                .unwrap_or(raw_text.as_str())
                .trim()
                .trim_start_matches('-')
                .trim()
                .to_string();

            poll_options.push(PollOption {
                timeline_ordinal: ordinal,
                option_index: index as i64,
                text: text_without_details,
                voters: details
                    .as_deref()
                    .and_then(first_number)
                    .or_else(|| first_number(&raw_text)),
                chosen: details
                    .as_deref()
                    .map(|details| details.to_ascii_lowercase().contains("chosen vote")),
                extra_json: json!({ "details": details }),
            });
        }
    }
}

fn parse_reactions(body: ElementRef<'_>) -> Value {
    let reactions: Vec<Value> = body
        .select(&selector(".reaction"))
        .filter_map(|reaction| {
            let emoji = select_text(reaction, ".emoji")?;
            Some(json!({
                "emoji": emoji,
                "count": select_text(reaction, ".count").and_then(|text| first_number(&text)),
            }))
        })
        .collect();

    json!(reactions)
}

fn select_element<'a>(element: ElementRef<'a>, selector_text: &str) -> Option<ElementRef<'a>> {
    element.select(&selector(selector_text)).next()
}

fn select_text(element: ElementRef<'_>, selector_text: &str) -> Option<String> {
    select_element(element, selector_text)
        .map(text_from_element)
        .and_then(non_empty)
}

fn select_attr(element: ElementRef<'_>, selector_text: &str, attr: &str) -> Option<String> {
    select_element(element, selector_text)
        .and_then(|element| element.value().attr(attr))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn first_number(input: &str) -> Option<i64> {
    let mut digits = String::new();
    let mut started = false;

    for character in input.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            started = true;
        } else if started {
            break;
        }
    }

    (!digits.is_empty())
        .then_some(digits)
        .and_then(|digits| digits.parse::<i64>().ok())
}

fn select_document_text(document: &Html, selector_text: &str) -> Option<String> {
    document
        .select(&selector(selector_text))
        .next()
        .map(text_from_element)
        .and_then(non_empty)
}

fn parse_optional_timestamp(
    timestamp: Option<&str>,
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    ordinal: i64,
    label: &str,
) -> Option<String> {
    let timestamp = timestamp?;
    match parse_telegram_timestamp(timestamp) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            push_warning(
                warnings,
                source_file_parse_order,
                Some(ordinal),
                WarningCode::MalformedTimestamp,
                format!("malformed {label}"),
                json!({ "timestamp": timestamp, "error": error.to_string() }),
            );
            None
        }
    }
}

fn direct_child_with_class<'a>(
    element: ElementRef<'a>,
    class_name: &str,
) -> Option<ElementRef<'a>> {
    element.children().find_map(|child| {
        let child = ElementRef::wrap(child)?;
        has_class(child, class_name).then_some(child)
    })
}

fn joined_export_path(parent: &Path, href: &str) -> PathBuf {
    let href = href
        .split(['?', '#'])
        .next()
        .unwrap_or(href)
        .trim_start_matches("./");
    parent.join(href)
}

fn media_kind(anchor: ElementRef<'_>) -> &'static str {
    let href = anchor.value().attr("href").unwrap_or_default();
    let title = select_text(anchor, ".title").unwrap_or_default();

    if has_class(anchor, "sticker_wrap") || looks_like_sticker_attachment(href, &title) {
        "sticker"
    } else if has_class(anchor, "animated_wrap") || title.eq_ignore_ascii_case("animation") {
        "animation"
    } else if has_class(anchor, "photo_wrap") || has_class(anchor, "media_photo") {
        "photo"
    } else if has_class(anchor, "video_file_wrap") || has_class(anchor, "media_video") {
        "video_file"
    } else if has_class(anchor, "media_audio_file") {
        "audio"
    } else if has_class(anchor, "media_voice_message") {
        "voice"
    } else {
        "file"
    }
}

fn looks_like_sticker_attachment(href: &str, title: &str) -> bool {
    let lower_href = href.to_ascii_lowercase();
    title.eq_ignore_ascii_case("sticker")
        || lower_href.starts_with("stickers/")
        || lower_href.contains("/stickers/")
        || lower_href.ends_with(".tgs")
}

fn mime_from_href(href: &str) -> Option<String> {
    mime_guess::from_path(href)
        .first()
        .map(|mime| mime.essence_str().to_string())
}

fn parse_file_size(status: &str) -> Option<u64> {
    // Telegram Desktop statuses are "<duration>, <size>" (e.g. "03:12, 7.3 MB") or a bare
    // size, always with a one-decimal magnitude. Match the numeric-plus-unit portion so
    // the duration's leading digits are never mistaken for the size, and keep the decimal.
    static SIZE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(TB|GB|MB|KB|B)\b").expect("file size regex compiles")
    });

    let captures = SIZE.captures_iter(status).last()?;
    let value: f64 = captures.get(1)?.as_str().parse().ok()?;
    let multiplier: f64 = match captures.get(2)?.as_str().to_ascii_uppercase().as_str() {
        "TB" => 1024_f64.powi(4),
        "GB" => 1024_f64.powi(3),
        "MB" => 1024_f64.powi(2),
        "KB" => 1024_f64,
        "B" => 1.0,
        _ => return None,
    };

    Some((value * multiplier).round() as u64)
}

fn parse_duration_text(text: &str) -> Option<i64> {
    // The colon token can carry trailing punctuation from the combined status, e.g. the
    // "03:12," in "03:12, 7.3 MB"; strip anything that is not a digit or a colon first.
    text.split_whitespace()
        .map(|part| {
            part.trim_matches(|character: char| !character.is_ascii_digit() && character != ':')
        })
        .find(|part| part.contains(':'))
        .and_then(|part| parse_duration_seconds(part).ok())
}

fn parse_image_dimensions(anchor: ElementRef<'_>) -> (Option<i64>, Option<i64>) {
    let Some(image) = select_element(anchor, "img") else {
        return (None, None);
    };
    let width = image
        .value()
        .attr("width")
        .and_then(first_number)
        .or_else(|| parse_style_dimension(image.value().attr("style"), "width"));
    let height = image
        .value()
        .attr("height")
        .and_then(first_number)
        .or_else(|| parse_style_dimension(image.value().attr("style"), "height"));

    (width, height)
}

fn parse_style_dimension(style: Option<&str>, property: &str) -> Option<i64> {
    style.and_then(|style| {
        style.split(';').find_map(|part| {
            let (name, value) = part.split_once(':')?;
            (name.trim().eq_ignore_ascii_case(property))
                .then(|| first_number(value))
                .flatten()
        })
    })
}

fn push_warning(
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    timeline_ordinal: Option<i64>,
    code: WarningCode,
    message: impl Into<String>,
    context: Value,
) {
    warnings.push(ImportWarning {
        source_file_parse_order: Some(source_file_parse_order),
        timeline_ordinal,
        code,
        message: message.into(),
        context,
    });
}

fn source_anchor(element: ElementRef<'_>) -> Option<String> {
    element.value().attr("id").map(str::to_string)
}

fn class_attr(element: ElementRef<'_>) -> Option<&str> {
    element.value().attr("class")
}

fn has_class(element: ElementRef<'_>, class_name: &str) -> bool {
    class_attr(element)
        .is_some_and(|classes| classes.split_whitespace().any(|class| class == class_name))
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn selector(selector_text: &str) -> Selector {
    // Parse each distinct CSS selector once and cache it: the per-message parse
    // path evaluates ~12-25 selectors, and re-parsing each on every call (a full
    // tokenize + compile) dominated the parser's cost. The distinct selector set
    // is tiny and fixed, so the cache never grows unbounded. Cloning a compiled
    // selector is far cheaper than re-parsing one.
    static CACHE: LazyLock<Mutex<HashMap<String, Selector>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut cache = CACHE.lock().expect("selector cache is not poisoned");
    if let Some(selector) = cache.get(selector_text) {
        return selector.clone();
    }
    let selector = Selector::parse(selector_text).expect("parser selector compiles");
    cache.insert(selector_text.to_string(), selector.clone());
    selector
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::model::{TimelineItemKind, WarningCode};

    #[test]
    fn parses_basic_export_fixture() {
        let parsed = parse_export_file(
            Path::new("tests/fixtures/basic_export"),
            Path::new("tests/fixtures/basic_export/chat_001/messages.html"),
            Path::new("chat_001/messages.html"),
            0,
            0,
        )
        .unwrap();

        assert_eq!(parsed.chat.unwrap().title, "Family Chat");
        assert!(
            parsed
                .timeline_items
                .iter()
                .all(|item| item.kind != TimelineItemKind::DateSeparator)
        );
        assert_eq!(parsed.messages.len(), 4);
        assert_eq!(parsed.service_events[0].event_type, "invite_members");
        assert!(
            parsed
                .messages
                .iter()
                .any(|message| message.sender_inferred)
        );
        assert!(
            parsed
                .attachments
                .iter()
                .any(|attachment| attachment.kind == "photo")
        );
        assert!(
            parsed
                .attachments
                .iter()
                .any(|attachment| attachment.kind == "file")
        );
    }

    fn approx_bytes(value: f64, unit: u64) -> u64 {
        (value * unit as f64).round() as u64
    }

    #[test]
    fn parses_file_size_from_real_status_strings() {
        let mib = 1024 * 1024;
        // Telegram Desktop's real status is "<duration>, <size>" with a one-decimal size.
        assert_eq!(parse_file_size("7.3 MB"), Some(approx_bytes(7.3, mib)));
        assert_eq!(
            parse_file_size("03:12, 7.3 MB"),
            Some(approx_bytes(7.3, mib))
        );
        assert_eq!(
            parse_file_size("00:04, 12.4 KB"),
            Some(approx_bytes(12.4, 1024))
        );
        assert_eq!(parse_file_size("512 B"), Some(512));
    }

    #[test]
    fn parses_duration_from_real_status_strings() {
        assert_eq!(parse_duration_text("03:12, 7.3 MB"), Some(192));
        assert_eq!(parse_duration_text("00:04, 12.4 KB"), Some(4));
        assert_eq!(parse_duration_text("1:01:40"), Some(3700));
    }

    #[test]
    fn parses_real_tdesktop_media_and_service_markup() {
        let parsed = parse_export_file(
            Path::new("tests/fixtures/tdesktop_media"),
            Path::new("tests/fixtures/tdesktop_media/chat_001/messages.html"),
            Path::new("chat_001/messages.html"),
            0,
            0,
        )
        .unwrap();

        // Voice notes use the real class token `media_voice_message`.
        let voice = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "voice")
            .expect("voice note recognized");
        assert_eq!(voice.duration_seconds, Some(4));

        // Audio size and duration come from the real "03:12, 7.3 MB" status string.
        let audio = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "audio")
            .expect("audio file recognized");
        assert_eq!(audio.duration_seconds, Some(192));
        assert_eq!(audio.file_size, Some(approx_bytes(7.3, 1024 * 1024)));

        // Video duration comes from the `.video_duration` overlay div.
        let video = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "video_file")
            .expect("video file recognized");
        assert_eq!(video.duration_seconds, Some(19));

        assert!(
            parsed
                .attachments
                .iter()
                .all(|attachment| attachment.kind != "unsupported")
        );

        // A genuine service event whose text contains a month + year must not be
        // swallowed by the date-separator heuristic.
        assert!(parsed.service_events.iter().any(|event| {
            event.event_type == "edit_group_title" && event.display_text.contains("March 2025 Trip")
        }));

        // The real negative-id date separator is still dropped, not stored.
        assert!(
            parsed
                .timeline_items
                .iter()
                .all(|item| item.display_text.as_deref() != Some("13 February 2025"))
        );

        // Group-photo and voice-chat events classify structurally, without phantom members.
        assert!(
            parsed
                .service_events
                .iter()
                .any(|event| event.event_type == "delete_group_photo")
        );
        assert!(
            parsed
                .service_events
                .iter()
                .any(|event| event.event_type == "group_call")
        );
        assert!(
            parsed
                .service_events
                .iter()
                .all(|event| event.target_names != vec!["group photo".to_string()])
        );
    }

    #[test]
    fn parses_second_fixture_with_poll_and_unknown_service_warning() {
        let parsed = parse_export_file(
            Path::new("tests/fixtures/basic_export"),
            Path::new("tests/fixtures/basic_export/chat_001/messages2.html"),
            Path::new("chat_001/messages2.html"),
            1,
            10,
        )
        .unwrap();

        assert_eq!(parsed.chat.unwrap().title, "Family Chat");
        assert!(
            parsed
                .timeline_items
                .iter()
                .all(|item| item.kind != TimelineItemKind::DateSeparator)
        );
        assert_eq!(parsed.polls.len(), 1);
        assert_eq!(parsed.poll_options.len(), 2);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.code == WarningCode::UnknownServiceEvent)
        );
    }

    #[test]
    fn skips_html_date_separator_services() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message service" id="message-1">
               <div class="body details">February 9, 2023</div>
              </div>
              <div class="message default clearfix" id="message1">
               <div class="body">
                <div class="pull_right date details" title="09.02.2023 20:56:46 UTC+03:00">20:56</div>
                <div class="from_name">Alice</div>
                <div class="text">Real message</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert_eq!(parsed.timeline_items.len(), 1);
        assert_eq!(parsed.timeline_items[0].kind, TimelineItemKind::Message);
        assert_eq!(parsed.messages.len(), 1);
        assert!(parsed.service_events.is_empty());
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn does_not_infer_sender_for_non_joined_message_without_from_name() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message1">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="text">First</div>
               </div>
              </div>
              <div class="message default clearfix" id="message2">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:38:01 UTC">08:38</div>
                <div class="text">No sender row</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        let second = parsed
            .messages
            .iter()
            .find(|message| message.telegram_message_id == Some(2))
            .unwrap();
        assert_eq!(second.sender_name, None);
        assert!(!second.sender_inferred);
        assert!(
            parsed
                .warnings
                .iter()
                .all(|warning| warning.code != WarningCode::InferredSender)
        );
    }

    #[test]
    fn message_without_id_is_stored_as_none_not_fabricated() {
        // A message element with no `id="messageN"` must be stored with
        // telegram_message_id == None, not a stand-in invented from its ordinal.
        let parsed = parse_inline_export(
            r#"
            <html><body>
             <div class="page_header"><div class="content"><div class="text bold">Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix">
               <div class="body">
                <div class="from_name">Alice</div>
                <div class="text">no id here</div>
               </div>
              </div>
             </div>
            </body></html>
            "#,
        );
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].telegram_message_id, None);
        assert_eq!(parsed.timeline_items[0].telegram_message_id, None);
    }

    #[test]
    fn extracts_reply_id_after_go_to_message_anchor() {
        let parsed = parse_inline_export(
            r##"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message202">
               <div class="body">
                <div class="pull_right date details" title="13.02.2025 11:00:00 UTC">11:00</div>
                <div class="from_name">Bob</div>
                <div class="reply_to details">In reply to <a href="messages2.html#go_to_message201">this message</a></div>
                <div class="text">Reply</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "##,
        );

        assert_eq!(parsed.messages[0].reply_to_message_id, Some(201));
    }

    #[test]
    fn parses_edited_timestamp_from_edited_marker_title() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message301">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">
                 <span class="edited" title="12.02.2025 08:40:00 UTC">edited</span>
                 08:37
                </div>
                <div class="from_name">Alice</div>
                <div class="text">Edited text</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert_eq!(
            parsed.messages[0].edited_timestamp.as_deref(),
            Some("2025-02-12T08:40:00Z")
        );
    }

    #[test]
    fn preserves_sender_link_metadata_in_message_extra_json() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message302">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name"><a href="tg://user?id=12345" data-user-id="12345">Alice</a></div>
                <div class="text">With sender id</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        let sender = &parsed.messages[0].extra_json["sender"];
        assert_eq!(sender["href"].as_str(), Some("tg://user?id=12345"));
        assert_eq!(sender["data_user_id"].as_str(), Some("12345"));
        assert_eq!(sender["user_id"].as_str(), Some("12345"));
    }

    #[test]
    fn preserves_raw_markup_for_reconstructable_message_and_service_details() {
        let parsed = parse_inline_export(
            r##"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message401">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">
                 <span class="edited" title="12.02.2025 08:40:00 UTC">edited</span>
                 08:37
                </div>
                <div class="from_name"><a href="tg://user?id=12345" data-user-id="12345">Alice</a></div>
                <div class="reply_to details">In reply to <a href="messages2.html#go_to_message201" onclick="return GoToMessage(201)">this message</a></div>
                <div class="forwarded body"><div class="from_name">Carol <span class="date details" title="11.02.2025 22:00:00 UTC">date</span></div></div>
                <div class="text">Line one<br>Line <strong>two</strong></div>
               </div>
              </div>
              <div class="message service" id="message402">
               <div class="body details"><a href="tg://user?id=12345">Alice</a> invited <a data-user-id="67890">Bob</a></div>
              </div>
             </div>
            </body>
            </html>
            "##,
        );

        let message_timeline = parsed
            .timeline_items
            .iter()
            .find(|item| item.telegram_message_id == Some(401))
            .unwrap();
        assert!(
            message_timeline.extra_json["source_html"]
                .as_str()
                .unwrap()
                .contains("onclick=\"return GoToMessage(201)\"")
        );

        let message = parsed
            .messages
            .iter()
            .find(|message| message.telegram_message_id == Some(401))
            .unwrap();
        assert!(
            message.extra_json["body_html"]
                .as_str()
                .unwrap()
                .contains("messages2.html#go_to_message201")
        );
        assert!(
            message.extra_json["text_html"]
                .as_str()
                .unwrap()
                .contains("<br")
        );
        assert!(
            message.extra_json["edited_html"]
                .as_str()
                .unwrap()
                .contains("title=\"12.02.2025 08:40:00 UTC\"")
        );
        assert!(
            message.extra_json["forwarded_html"]
                .as_str()
                .unwrap()
                .contains("11.02.2025 22:00:00 UTC")
        );

        let service = parsed
            .service_events
            .iter()
            .find(|event| event.timeline_ordinal == 1)
            .unwrap();
        assert!(
            service.extra_json["body_html"]
                .as_str()
                .unwrap()
                .contains("data-user-id=\"67890\"")
        );
    }

    #[test]
    fn warns_and_preserves_unsupported_media_shape() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <div class="sticker_wrap"><img src="stickers/sticker.webp" alt="wave"/></div>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.code == WarningCode::UnsupportedMediaShape)
        );
        let unsupported = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "unsupported")
            .unwrap();
        assert_eq!(
            unsupported.skip_reason.as_deref(),
            Some("unsupported_media_shape")
        );
        assert!(
            unsupported.extra_json["source_html"]
                .as_str()
                .unwrap()
                .contains("sticker_wrap")
        );
    }

    #[test]
    fn parses_media_photo_anchor_as_photo_attachment() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <a class="media clearfix pull_left block_link media_photo" href="photos/photo_1.jpg">
                  <div class="body">
                   <div class="title bold">photo_1.jpg</div>
                  </div>
                 </a>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert!(
            parsed
                .warnings
                .iter()
                .all(|warning| warning.code != WarningCode::UnsupportedMediaShape)
        );
        let attachment = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "photo")
            .unwrap();
        assert_eq!(
            attachment.relative_path.as_deref(),
            Some(Path::new("chat_001/photos/photo_1.jpg"))
        );
        assert_eq!(attachment.title.as_deref(), Some("photo_1.jpg"));
    }

    #[test]
    fn parses_sticker_anchor_as_sticker_attachment() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <a class="sticker_wrap clearfix pull_left" href="stickers/sticker.webp">
                  <img style="width: 192px; height: 190px" class="sticker" src="stickers/sticker_thumb.webp">
                 </a>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert!(
            parsed
                .warnings
                .iter()
                .all(|warning| warning.code != WarningCode::UnsupportedMediaShape)
        );
        let attachment = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "sticker")
            .unwrap();
        assert_eq!(
            attachment.relative_path.as_deref(),
            Some(Path::new("chat_001/stickers/sticker.webp"))
        );
        assert_eq!(
            attachment.thumbnail_path.as_deref(),
            Some(Path::new("chat_001/stickers/sticker_thumb.webp"))
        );
        assert_eq!(attachment.width, Some(192));
        assert_eq!(attachment.height, Some(190));
    }

    #[test]
    fn parses_media_photo_sticker_file_as_sticker_attachment() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <a class="media clearfix pull_left block_link media_photo" href="stickers/AnimatedSticker.tgs">
                  <div class="body">
                   <div class="title bold">Sticker</div>
                   <div class="status details">laughing</div>
                  </div>
                 </a>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        let attachment = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "sticker")
            .unwrap();
        assert_eq!(
            attachment.relative_path.as_deref(),
            Some(Path::new("chat_001/stickers/AnimatedSticker.tgs"))
        );
    }

    #[test]
    fn parses_animated_anchor_as_animation_attachment() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <a class="animated_wrap clearfix pull_left" href="video_files/animation.gif.mp4">
                  <div class="video_play_bg"><div class="gif_play">GIF</div></div>
                  <img style="width: 130px; height: 100px" src="video_files/animation.gif.mp4_thumb.jpg" class="animated">
                 </a>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        assert!(
            parsed
                .warnings
                .iter()
                .all(|warning| warning.code != WarningCode::UnsupportedMediaShape)
        );
        let attachment = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "animation")
            .unwrap();
        assert_eq!(
            attachment.relative_path.as_deref(),
            Some(Path::new("chat_001/video_files/animation.gif.mp4"))
        );
        assert_eq!(
            attachment.thumbnail_path.as_deref(),
            Some(Path::new(
                "chat_001/video_files/animation.gif.mp4_thumb.jpg"
            ))
        );
        assert_eq!(attachment.width, Some(130));
        assert_eq!(attachment.height, Some(100));
    }

    #[test]
    fn parses_video_file_anchor_as_video_file_attachment() {
        let parsed = parse_inline_export(
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message501">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="media_wrap clearfix">
                 <a class="video_file_wrap clearfix pull_left" href="video_files/video.mp4">
                  <img class="video_file" src="video_files/video.mp4_thumb.jpg" style="width: 146px; height: 260px"/>
                 </a>
                </div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        );

        let attachment = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "video_file")
            .unwrap();
        assert_eq!(
            attachment.relative_path.as_deref(),
            Some(Path::new("chat_001/video_files/video.mp4"))
        );
    }

    #[test]
    fn stateful_parsing_infers_joined_sender_across_files() {
        let temp = tempfile::tempdir().unwrap();
        let chat_dir = temp.path().join("chat_001");
        fs::create_dir_all(&chat_dir).unwrap();
        let first_path = chat_dir.join("messages.html");
        let second_path = chat_dir.join("messages2.html");

        fs::write(
            &first_path,
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message601">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="text">First file</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        )
        .unwrap();
        fs::write(
            &second_path,
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix joined" id="message602">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:38:01 UTC">08:38</div>
                <div class="text">Second file joined line</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        )
        .unwrap();

        let mut state = ParserState::default();
        parse_export_file_with_state(
            temp.path(),
            &first_path,
            Path::new("chat_001/messages.html"),
            0,
            0,
            &mut state,
        )
        .unwrap();
        let second = parse_export_file_with_state(
            temp.path(),
            &second_path,
            Path::new("chat_001/messages2.html"),
            1,
            10,
            &mut state,
        )
        .unwrap();

        assert_eq!(second.messages[0].sender_name.as_deref(), Some("Alice"));
        assert!(second.messages[0].sender_inferred);
        assert!(
            second
                .warnings
                .iter()
                .any(|warning| warning.code == WarningCode::InferredSender)
        );
    }

    fn parse_inline_export(html: &str) -> ParsedExport {
        let temp = tempfile::tempdir().unwrap();
        let chat_dir = temp.path().join("chat_001");
        fs::create_dir_all(&chat_dir).unwrap();
        let absolute_path = chat_dir.join("messages.html");
        fs::write(&absolute_path, html).unwrap();

        parse_export_file(
            temp.path(),
            &absolute_path,
            Path::new("chat_001/messages.html"),
            0,
            0,
        )
        .unwrap()
    }
}
