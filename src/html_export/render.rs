use crate::{
    error::Result,
    export_rows::{
        AttachmentRow, ExportRows, MessageRow, PollOptionRow, PollRow, ServiceEventRow, TimelineRow,
    },
    media_path::{safe_href, safe_media_path},
    model::{TextEntity, TextEntityKind},
    time::parse_utc,
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::Value;
use std::collections::HashMap;

pub fn escape_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '\n' | '\u{2028}' | '\u{2029}' => output.push_str("<br>"),
            '"' => output.push_str("&quot;"),
            '&' => output.push_str("&amp;"),
            '\'' => output.push_str("&apos;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            character if character < ' ' => {
                output.push_str(&format!("&#x{:02X};", character as u32));
            }
            character => output.push(character),
        }
    }
    output
}

pub fn format_date_text(date: DateTime<Utc>) -> String {
    format!(
        "{} {} {}",
        date.day(),
        month_name(date.month()),
        date.year()
    )
}

pub fn format_time_text(date: DateTime<Utc>) -> String {
    format!("{:02}:{:02}", date.hour(), date.minute())
}

pub fn format_title_timestamp(date: DateTime<Utc>) -> String {
    format!(
        "{:02}.{:02}.{:04} {:02}:{:02}:{:02} UTC",
        date.day(),
        date.month(),
        date.year(),
        date.hour(),
        date.minute(),
        date.second()
    )
}

pub fn render_text_entities(entities: &[TextEntity]) -> String {
    entities.iter().map(render_text_entity).collect::<String>()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedHistory {
    pub html: String,
    pub generated_date_separators: usize,
}

struct RenderedMessage {
    html: String,
    info: RenderedMessageInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedMessageInfo {
    sender_name: Option<String>,
    via_bot: Option<String>,
    date: DateTime<Utc>,
    forwarded_from: Option<String>,
    forwarded_from_id: Option<String>,
    forwarded_date: Option<DateTime<Utc>>,
}

pub fn render_history(rows: &ExportRows) -> Result<RenderedHistory> {
    let messages_by_timeline_id = rows
        .messages
        .iter()
        .map(|message| (message.timeline_item_id, message))
        .collect::<HashMap<_, _>>();
    let service_events_by_timeline_id: HashMap<i64, &ServiceEventRow> = rows
        .service_events
        .iter()
        .map(|service_event| (service_event.timeline_item_id, service_event))
        .collect::<HashMap<_, _>>();
    let mut attachments_by_timeline_id: HashMap<i64, Vec<&AttachmentRow>> = HashMap::new();
    for attachment in &rows.attachments {
        attachments_by_timeline_id
            .entry(attachment.timeline_item_id)
            .or_default()
            .push(attachment);
    }
    let polls_by_timeline_id = rows
        .polls
        .iter()
        .map(|poll| (poll.timeline_item_id, poll))
        .collect::<HashMap<_, _>>();
    let mut poll_options_by_poll_id: HashMap<i64, Vec<&PollOptionRow>> = HashMap::new();
    for option in &rows.poll_options {
        poll_options_by_poll_id
            .entry(option.poll_id)
            .or_default()
            .push(option);
    }

    let mut html = String::new();
    let mut previous_calendar_date = None;
    let mut previous_message_info = None;
    let mut generated_date_separators = 0;
    let mut next_date_separator_id = -1;

    for item in &rows.timeline_items {
        if item.item_kind == "date_separator" {
            previous_message_info = None;
            continue;
        }

        let timestamp = item.timestamp.as_deref().map(parse_utc).transpose()?;
        if let Some(timestamp) = timestamp {
            let calendar_date = timestamp.date_naive();
            if previous_calendar_date != Some(calendar_date) {
                html.push_str(&render_service_block(
                    next_date_separator_id,
                    &format_date_text(timestamp),
                ));
                next_date_separator_id -= 1;
                generated_date_separators += 1;
                previous_calendar_date = Some(calendar_date);
                previous_message_info = None;
            }
        }

        match item.item_kind.as_str() {
            "message" => {
                if let Some(message) = messages_by_timeline_id.get(&item.id) {
                    if let Some(timestamp) = timestamp {
                        let attachments = attachments_by_timeline_id
                            .get(&item.id)
                            .map_or(&[][..], Vec::as_slice);
                        let poll = polls_by_timeline_id.get(&item.id).copied();
                        let poll_options = poll
                            .and_then(|poll| poll_options_by_poll_id.get(&poll.id))
                            .map_or(&[][..], Vec::as_slice);
                        let rendered = render_message(
                            item,
                            message,
                            timestamp,
                            previous_message_info.as_ref(),
                            attachments,
                            poll,
                            poll_options,
                        )?;
                        html.push_str(&rendered.html);
                        previous_message_info = Some(rendered.info);
                    } else {
                        let attachments = attachments_by_timeline_id
                            .get(&item.id)
                            .map_or(&[][..], Vec::as_slice);
                        let poll = polls_by_timeline_id.get(&item.id).copied();
                        let poll_options = poll
                            .and_then(|poll| poll_options_by_poll_id.get(&poll.id))
                            .map_or(&[][..], Vec::as_slice);
                        html.push_str(&render_message_without_timestamp(
                            item,
                            message,
                            attachments,
                            poll,
                            poll_options,
                        )?);
                        previous_message_info = None;
                    }
                }
            }
            "service_event" => {
                let id = item.telegram_message_id.unwrap_or(item.ordinal);
                if let Some(service_event) = service_events_by_timeline_id.get(&item.id) {
                    let display_text = service_event_display_text(service_event);
                    html.push_str(&render_service_block(id, &display_text));
                } else if let Some(display_text) = &item.display_text {
                    html.push_str(&render_service_block(id, display_text));
                }
                previous_message_info = None;
            }
            "unsupported" => {
                if let Some(display_text) = &item.display_text {
                    let id = item.telegram_message_id.unwrap_or(item.ordinal);
                    html.push_str(&render_service_block(id, display_text));
                }
                previous_message_info = None;
            }
            _ => {
                if let Some(display_text) = &item.display_text {
                    let id = item.telegram_message_id.unwrap_or(item.ordinal);
                    html.push_str(&render_service_block(id, display_text));
                }
                previous_message_info = None;
            }
        }
    }

    Ok(RenderedHistory {
        html,
        generated_date_separators,
    })
}

pub fn render_page(chat_title: &str, history_html: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n <head>\n  <meta charset=\"utf-8\"/>\n  <title>Exported Data</title>\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\"/>\n  <link href=\"css/style.css\" rel=\"stylesheet\"/>\n  <script src=\"js/script.js\" type=\"text/javascript\"></script>\n </head>\n <body onload=\"CheckLocation();\">\n  <div class=\"page_wrap\">\n   <div class=\"page_header\">\n    <div class=\"content\">\n     <div class=\"text bold\">{}</div>\n    </div>\n   </div>\n   <div class=\"page_body chat_page\">\n    <div class=\"history\">{}\n    </div>\n   </div>\n  </div>\n </body>\n</html>\n",
        escape_html(chat_title),
        history_html
    )
}

fn render_service_block(id: i64, text: &str) -> String {
    format!(
        "\n    <div class=\"message service\" id=\"message{id}\">\n     <div class=\"body details\">\n      {}\n     </div>\n    </div>\n",
        escape_html(text)
    )
}

fn service_event_display_text(service_event: &ServiceEventRow) -> String {
    match service_event.event_type.as_str() {
        "invite_members" => {
            let targets = service_event_target_names(service_event);
            match (service_event_actor_name(service_event), targets.is_empty()) {
                (Some(actor), false) => format!("{actor} invited {}", targets.join(", ")),
                _ => service_event.display_text.clone(),
            }
        }
        "remove_members" => {
            let targets = service_event_target_names(service_event);
            match (service_event_actor_name(service_event), targets.is_empty()) {
                (Some(actor), false) => format!("{actor} removed {}", targets.join(", ")),
                _ => service_event.display_text.clone(),
            }
        }
        "edit_group_title" => match (
            service_event_actor_name(service_event),
            service_event_extra_string(service_event, "title"),
        ) {
            (Some(actor), Some(title)) => format!("{actor} changed group title to «{title}»"),
            _ => service_event.display_text.clone(),
        },
        "migrate_from_group" => match (
            service_event_actor_name(service_event),
            service_event_extra_string(service_event, "title"),
        ) {
            (Some(actor), Some(title)) => {
                format!("{actor} converted a basic group to this supergroup «{title}»")
            }
            _ => service_event.display_text.clone(),
        },
        "pin_message" => match service_event_actor_name(service_event) {
            Some(actor) => format!("{actor} pinned this message"),
            None => service_event.display_text.clone(),
        },
        "group_call" => match service_event_actor_name(service_event) {
            Some(actor) => format!("{actor} started voice chat"),
            None => "Voice chat".to_string(),
        },
        _ => service_event.display_text.clone(),
    }
}

fn service_event_actor_name(service_event: &ServiceEventRow) -> Option<&str> {
    service_event
        .actor_name
        .as_deref()
        .filter(|actor| !actor.is_empty())
}

fn service_event_target_names(service_event: &ServiceEventRow) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&service_event.target_names_json)
        .unwrap_or_default()
        .into_iter()
        .filter(|target| !target.is_empty())
        .collect()
}

fn service_event_extra_string(service_event: &ServiceEventRow, key: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(&service_event.extra_json).ok()?;
    value
        .get(key)
        .or_else(|| value.get("source_json").and_then(|source| source.get(key)))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn message_needs_wrap(
    message: &MessageRow,
    timestamp: DateTime<Utc>,
    previous: Option<&RenderedMessageInfo>,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    let current = rendered_message_info(message, timestamp);
    if previous.sender_name != current.sender_name
        || previous.via_bot != current.via_bot
        || previous.date.date_naive() != current.date.date_naive()
        || previous.forwarded_from != current.forwarded_from
        || previous.forwarded_from_id != current.forwarded_from_id
        || previous.forwarded_date != current.forwarded_date
    {
        return true;
    }

    let max_gap_seconds = if message.forwarded_from.is_some() && message.forwarded_from_id.is_none()
    {
        1
    } else {
        900
    };
    let gap_seconds = timestamp.signed_duration_since(previous.date).num_seconds();
    gap_seconds < 0 || gap_seconds > max_gap_seconds
}

fn render_message(
    item: &TimelineRow,
    message: &MessageRow,
    timestamp: DateTime<Utc>,
    previous: Option<&RenderedMessageInfo>,
    attachments: &[&AttachmentRow],
    poll: Option<&PollRow>,
    poll_options: &[&PollOptionRow],
) -> Result<RenderedMessage> {
    let wrap = message_needs_wrap(message, timestamp, previous);
    let html = render_message_html(
        item,
        message,
        Some(timestamp),
        wrap,
        attachments,
        poll,
        poll_options,
    )?;
    Ok(RenderedMessage {
        html,
        info: rendered_message_info(message, timestamp),
    })
}

fn render_message_without_timestamp(
    item: &TimelineRow,
    message: &MessageRow,
    attachments: &[&AttachmentRow],
    poll: Option<&PollRow>,
    poll_options: &[&PollOptionRow],
) -> Result<String> {
    render_message_html(item, message, None, true, attachments, poll, poll_options)
}

fn render_message_html(
    item: &TimelineRow,
    message: &MessageRow,
    timestamp: Option<DateTime<Utc>>,
    wrap: bool,
    attachments: &[&AttachmentRow],
    poll: Option<&PollRow>,
    poll_options: &[&PollOptionRow],
) -> Result<String> {
    let class_name = if wrap {
        "message default clearfix"
    } else {
        "message default clearfix joined"
    };
    let mut html = format!(
        "\n    <div class=\"{class_name}\" id=\"message{}\">\n",
        message.telegram_message_id
    );
    html.push_str("     <div class=\"body\">\n");

    if let Some(timestamp) = timestamp {
        html.push_str(&format!(
            "      <div class=\"pull_right date details\" title=\"{}\">{}</div>\n",
            escape_attr(&format_title_timestamp(timestamp)),
            escape_html(&format_time_text(timestamp))
        ));
    }

    if wrap {
        html.push_str(&render_from_name(item, message));
    }

    if let Some(forwarded_from) = &message.forwarded_from {
        html.push_str(&render_forwarded_block(
            forwarded_from,
            message.forwarded_date.as_deref(),
        ));
    }

    if message
        .reply_to_peer_id
        .as_deref()
        .is_some_and(|reply_peer_id| !reply_peer_id.is_empty())
    {
        html.push_str(
            "     <div class=\"reply_to details\">In reply to a message in another chat</div>\n",
        );
    } else if let Some(reply_to_message_id) = message.reply_to_message_id {
        html.push_str(&format!(
            "     <div class=\"reply_to details\">In reply to <a href=\"#go_to_message{reply_to_message_id}\" onclick=\"return GoToMessage({reply_to_message_id})\">this message</a></div>\n"
        ));
    }

    for attachment in attachments {
        html.push_str(&render_media_wrap(&render_attachment(attachment)));
    }

    if let Some(poll) = poll {
        html.push_str(&render_media_wrap(&render_poll(poll, poll_options)));
    }

    let text = render_message_text(message)?;
    if !text.is_empty() {
        html.push_str(&format!("      <div class=\"text\">{text}</div>\n"));
    }

    let buttons = render_inline_bot_buttons(&message.inline_bot_buttons_json);
    if !buttons.is_empty() {
        html.push_str(&buttons);
    }

    if let Some(author) = message.author.as_ref().filter(|author| !author.is_empty()) {
        html.push_str(&format!(
            "      <div class=\"signature details\">{}</div>\n",
            escape_html(author)
        ));
    }

    let reactions = render_reactions(&message.reactions_json);
    if !reactions.is_empty() {
        html.push_str(&reactions);
    }

    html.push_str("     </div>\n");
    html.push_str("    </div>\n");
    Ok(html)
}

fn rendered_message_info(message: &MessageRow, timestamp: DateTime<Utc>) -> RenderedMessageInfo {
    RenderedMessageInfo {
        sender_name: message.sender_name.clone(),
        via_bot: message.via_bot.clone(),
        date: timestamp,
        forwarded_from: message.forwarded_from.clone(),
        forwarded_from_id: message.forwarded_from_id.clone(),
        forwarded_date: message
            .forwarded_date
            .as_deref()
            .and_then(|date| parse_utc(date).ok()),
    }
}

fn render_from_name(item: &TimelineRow, message: &MessageRow) -> String {
    let sender_name = message
        .sender_name
        .as_deref()
        .or(item.actor_name.as_deref())
        .unwrap_or("Deleted Account");
    let mut html = format!(
        "      <div class=\"from_name\">{}</div>\n",
        escape_html(sender_name)
    );
    if let Some(via_bot) = message
        .via_bot
        .as_ref()
        .filter(|via_bot| !via_bot.is_empty())
    {
        html = format!(
            "      <div class=\"from_name\">{} <span class=\"details\">via {}</span></div>\n",
            escape_html(sender_name),
            escape_html(via_bot)
        );
    }
    html
}

fn render_forwarded_block(forwarded_from: &str, forwarded_date: Option<&str>) -> String {
    let mut date_html = String::new();
    if let Some(timestamp) = forwarded_date.and_then(|date| parse_utc(date).ok()) {
        let title = format_title_timestamp(timestamp);
        let visible = title.strip_suffix(" UTC").unwrap_or(&title);
        date_html = format!(
            " <span class=\"date details\" title=\"{}\"> {}</span>",
            escape_attr(&title),
            escape_html(visible)
        );
    }
    format!(
        "      <div class=\"forwarded body\">\n       <div class=\"from_name\">{}{date_html}</div>\n      </div>\n",
        escape_html(forwarded_from)
    )
}

fn render_message_text(message: &MessageRow) -> Result<String> {
    let entities = if message.text_entities_json.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str::<Vec<TextEntity>>(&message.text_entities_json).unwrap_or_default()
    };
    if entities.is_empty() {
        Ok(message
            .plain_text
            .as_deref()
            .map(escape_html)
            .unwrap_or_default())
    } else {
        Ok(render_text_entities(&entities))
    }
}

fn render_attachment(attachment: &AttachmentRow) -> String {
    match attachment.attachment_kind.as_str() {
        "photo" => render_photo_attachment(attachment),
        _ => render_file_attachment(attachment),
    }
}

fn render_media_wrap(inner_html: &str) -> String {
    format!("      <div class=\"media_wrap clearfix\">\n{inner_html}      </div>\n")
}

fn render_photo_attachment(attachment: &AttachmentRow) -> String {
    let title = attachment_title(attachment);
    let Some(path) = attachment_render_path(attachment) else {
        return format!(
            "       <div class=\"media clearfix pull_left media_photo\"><div class=\"body\"><div class=\"title bold\">{}</div>{}</div></div>\n",
            escape_html(&title),
            render_attachment_status(attachment)
        );
    };
    let escaped_path = escape_attr(&path);
    let dimensions = match (attachment.width, attachment.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => {
            format!(" width=\"{width}\" height=\"{height}\"")
        }
        _ => String::new(),
    };
    format!(
        "       <a class=\"photo_wrap clearfix pull_left\" href=\"{escaped_path}\"><img class=\"photo\" src=\"{escaped_path}\" alt=\"{}\"{dimensions}/></a>\n",
        escape_attr(&title)
    )
}

fn render_file_attachment(attachment: &AttachmentRow) -> String {
    let title = attachment_title(attachment);
    let status = render_attachment_status(attachment);
    let body = format!(
        "<div class=\"body\"><div class=\"title bold\">{}</div>{status}</div>",
        escape_html(&title)
    );
    if let Some(path) = attachment_render_path(attachment) {
        format!(
            "       <a class=\"media clearfix pull_left block_link media_file\" href=\"{}\">{body}</a>\n",
            escape_attr(&path)
        )
    } else {
        format!("       <div class=\"media clearfix pull_left media_file\">{body}</div>\n")
    }
}

fn attachment_render_path(attachment: &AttachmentRow) -> Option<String> {
    attachment_original_href(attachment).or_else(|| {
        attachment
            .relative_path
            .as_deref()
            .and_then(safe_media_path)
    })
}

fn attachment_original_href(attachment: &AttachmentRow) -> Option<String> {
    let value = serde_json::from_str::<Value>(&attachment.extra_json).ok()?;
    value.get("href")?.as_str().and_then(safe_media_path)
}

fn render_attachment_status(attachment: &AttachmentRow) -> String {
    let mut parts = Vec::new();
    if let Some(mime_type) = attachment
        .mime_type
        .as_ref()
        .filter(|mime_type| !mime_type.is_empty())
    {
        parts.push(mime_type.clone());
    }
    if let Some(file_size) = attachment.file_size {
        parts.push(format_file_size(file_size));
    }
    if let Some(duration_seconds) = attachment.duration_seconds {
        parts.push(format_duration(duration_seconds));
    }
    match (attachment.width, attachment.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => {
            parts.push(format!("{width}x{height}"));
        }
        _ => {}
    }
    if attachment.spoiler {
        parts.push("spoiler".to_string());
    }
    if let Some(ttl_seconds) = attachment.ttl_seconds {
        parts.push(format!("expires in {ttl_seconds}s"));
    }
    if let Some(skip_reason) = attachment
        .skip_reason
        .as_ref()
        .filter(|skip_reason| !skip_reason.is_empty())
    {
        parts.push(format!("skipped: {skip_reason}"));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"status details\">{}</div>",
            escape_html(&parts.join(", "))
        )
    }
}

fn attachment_title(attachment: &AttachmentRow) -> String {
    attachment
        .title
        .clone()
        .or_else(|| {
            attachment
                .relative_path
                .as_deref()
                .and_then(|path| path.rsplit(['/', '\\']).next())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| attachment.attachment_kind.clone())
}

fn format_file_size(bytes: i64) -> String {
    // Match Telegram Desktop's FormatSizeText: one truncated decimal for KB/MB so the
    // re-exported HTML round-trips back through the importer's size parser.
    let bytes = bytes.max(0);
    if bytes >= 1024 * 1024 {
        let tenths = bytes * 10 / (1024 * 1024);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else if bytes >= 1024 {
        let tenths = bytes * 10 / 1024;
        format!("{}.{} KB", tenths / 10, tenths % 10)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(seconds: i64) -> String {
    // Match Telegram Desktop's FormatDurationText: H:MM:SS (hours only when present),
    // minutes always two digits, so durations round-trip through the importer.
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

fn render_poll(poll: &PollRow, options: &[&PollOptionRow]) -> String {
    let mut sorted_options = options.to_vec();
    sorted_options.sort_by_key(|option| option.option_index);

    let result_kind = if poll.closed.unwrap_or(false) {
        "Closed poll"
    } else {
        "Anonymous poll"
    };
    let mut html = format!(
        "       <div class=\"media_poll\">\n        <div class=\"question bold\">{}</div>\n        <div class=\"details\">{result_kind}</div>\n",
        escape_html(&poll.question)
    );
    for option in sorted_options {
        let votes = option.voters.unwrap_or(0);
        let mut details = vec![vote_wording(votes)];
        if option.chosen.unwrap_or(false) {
            details.push("chosen vote".to_string());
        }
        html.push_str(&format!(
            "        <div class=\"answer\">- {} <span class=\"details\">{}</span></div>\n",
            escape_html(&option.text),
            escape_html(&details.join(", "))
        ));
    }
    if let Some(total_voters) = poll.total_voters {
        html.push_str(&format!(
            "        <div class=\"total details\">{} total</div>\n",
            escape_html(&vote_wording(total_voters))
        ));
    }
    html.push_str("       </div>\n");
    html
}

fn vote_wording(votes: i64) -> String {
    match votes {
        0 => "0 votes".to_string(),
        1 => "1 vote".to_string(),
        votes => format!("{votes} votes"),
    }
}

fn render_inline_bot_buttons(buttons_json: &str) -> String {
    if buttons_json.trim().is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(buttons_json) else {
        return String::new();
    };
    let rows = inline_button_rows(&value);
    if rows.is_empty() {
        return String::new();
    }

    let mut rendered_rows = Vec::new();
    for row in rows {
        let mut rendered_cells = Vec::new();
        for button in row {
            if let Some(text) = inline_button_text(button) {
                let title = inline_button_details(button);
                let title_attr = if title.is_empty() {
                    String::new()
                } else {
                    format!(" title=\"{}\"", escape_attr(&title))
                };
                rendered_cells.push(format!(
                    "<td><span class=\"bot_button\"{title_attr}>{}</span></td>",
                    escape_html(text)
                ));
            }
        }
        if !rendered_cells.is_empty() {
            rendered_rows.push(format!("<tr>{}</tr>", rendered_cells.join("")));
        }
    }

    if rendered_rows.is_empty() {
        String::new()
    } else {
        format!(
            "      <table class=\"bot_buttons_table\">{}</table>\n",
            rendered_rows.join("")
        )
    }
}

fn inline_button_rows(value: &Value) -> Vec<Vec<&Value>> {
    let Some(buttons) = value.as_array() else {
        return Vec::new();
    };
    if buttons.is_empty() {
        return Vec::new();
    }
    if buttons.iter().all(Value::is_array) {
        buttons
            .iter()
            .filter_map(Value::as_array)
            .map(|row| row.iter().collect())
            .collect()
    } else {
        vec![buttons.iter().collect()]
    }
}

fn inline_button_text(button: &Value) -> Option<&str> {
    button.as_str().or_else(|| {
        ["text", "label", "title"]
            .into_iter()
            .find_map(|key| button.get(key).and_then(Value::as_str))
    })
}

fn inline_button_details(button: &Value) -> String {
    let Some(object) = button.as_object() else {
        return String::new();
    };
    ["type", "data", "url"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(json_scalar_to_string)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn json_scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn render_reactions(reactions_json: &str) -> String {
    let Ok(Value::Array(reactions)) = serde_json::from_str::<Value>(reactions_json) else {
        return String::new();
    };
    let mut rendered = String::new();
    for reaction in reactions {
        let Some(emoji) = reaction_emoji(&reaction) else {
            continue;
        };
        let count = reaction_count(&reaction);
        rendered.push_str(&format!(
            "<span class=\"reaction\"><span class=\"emoji\">{}</span><span class=\"count\">{count}</span></span>",
            escape_html(&emoji)
        ));
    }
    if rendered.is_empty() {
        String::new()
    } else {
        format!("     <div class=\"reactions\">{rendered}</div>\n")
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

fn escape_attr(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '"' => output.push_str("&quot;"),
            '&' => output.push_str("&amp;"),
            '\'' => output.push_str("&apos;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\u{2028}' | '\u{2029}' => {
                output.push_str(&format!("&#x{:04X};", character as u32));
            }
            character if character.is_control() => {
                output.push_str(&format!("&#x{:02X};", character as u32));
            }
            character => output.push(character),
        }
    }
    output
}

fn escape_js_string(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\'' => output.push_str("\\'"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            '<' => output.push_str("\\x3C"),
            '>' => output.push_str("\\x3E"),
            '&' => output.push_str("\\x26"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04X}", character as u32));
            }
            character => output.push(character),
        }
    }
    output
}

fn render_onclick_link(function_name: &str, argument: &str, text: &str) -> String {
    let onclick = escape_attr(&format!(
        "return {function_name}(\"{}\")",
        escape_js_string(argument)
    ));
    format!("<a href=\"\" onclick=\"{onclick}\">{text}</a>")
}

fn render_safe_link(href: &str, text: &str) -> String {
    if let Some(href) = safe_href(href) {
        format!("<a href=\"{}\">{text}</a>", escape_attr(&href))
    } else {
        text.to_string()
    }
}

fn telegram_mention_href(username: &str) -> Option<String> {
    if username.is_empty()
        || username
            .chars()
            .any(|character| !character.is_ascii_alphanumeric() && character != '_')
    {
        return None;
    }
    Some(format!("https://t.me/{username}"))
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    }
}

fn render_text_entity(entity: &TextEntity) -> String {
    let text = escape_html(&entity.text);
    match &entity.kind {
        TextEntityKind::Text | TextEntityKind::Unknown | TextEntityKind::BankCard => text,
        TextEntityKind::Mention => {
            let username = entity.text.trim_start_matches('@');
            if let Some(href) = telegram_mention_href(username) {
                format!("<a href=\"{}\">{text}</a>", escape_attr(&href))
            } else {
                text
            }
        }
        TextEntityKind::Hashtag => {
            render_onclick_link("ShowHashtag", entity.text.trim_start_matches('#'), &text)
        }
        TextEntityKind::BotCommand => {
            render_onclick_link("ShowBotCommand", entity.text.trim_start_matches('/'), &text)
        }
        TextEntityKind::Url => render_safe_link(&entity.text, &text),
        TextEntityKind::Email => {
            format!(
                "<a href=\"{}\">{text}</a>",
                escape_attr(&format!("mailto:{}", entity.text))
            )
        }
        TextEntityKind::Bold => format!("<strong>{text}</strong>"),
        TextEntityKind::Italic => format!("<em>{text}</em>"),
        TextEntityKind::Code => format!("<code>{text}</code>"),
        TextEntityKind::Pre => format!("<pre>{text}</pre>"),
        TextEntityKind::TextUrl => {
            if let Some(href) = entity
                .extra
                .get("href")
                .and_then(Value::as_str)
                .and_then(safe_href)
            {
                format!("<a href=\"{}\">{text}</a>", escape_attr(&href))
            } else {
                text
            }
        }
        TextEntityKind::MentionName => {
            let onclick = escape_attr("return ShowMentionName()");
            format!("<a href=\"\" onclick=\"{onclick}\">{text}</a>")
        }
        TextEntityKind::Phone => {
            format!(
                "<a href=\"{}\">{text}</a>",
                escape_attr(&format!("tel:{}", entity.text))
            )
        }
        TextEntityKind::Cashtag => {
            render_onclick_link("ShowCashtag", entity.text.trim_start_matches('$'), &text)
        }
        TextEntityKind::Underline => format!("<u>{text}</u>"),
        TextEntityKind::Strike => format!("<s>{text}</s>"),
        TextEntityKind::Blockquote => format!("<blockquote>{text}</blockquote>"),
        TextEntityKind::Spoiler => format!(
            "<span class=\"spoiler hidden\" onclick=\"ShowSpoiler(this)\"><span aria-hidden=\"true\">{text}</span></span>"
        ),
        TextEntityKind::CustomEmoji => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_size_uses_one_decimal_like_telegram() {
        assert_eq!(format_file_size(7654605), "7.3 MB");
        assert_eq!(format_file_size(1990000), "1.8 MB");
        assert_eq!(format_file_size(12698), "12.4 KB");
        assert_eq!(format_file_size(512), "512 B");
    }

    #[test]
    fn duration_uses_colon_format_like_telegram() {
        assert_eq!(format_duration(19), "00:19");
        assert_eq!(format_duration(192), "03:12");
        assert_eq!(format_duration(3700), "1:01:40");
    }

    #[test]
    fn escapes_like_telegram_desktop_html() {
        assert_eq!(
            escape_html("A\n\"&'<>B\u{2028}C"),
            "A<br>&quot;&amp;&apos;&lt;&gt;B<br>C"
        );
    }

    #[test]
    fn renders_text_entities_like_telegram_desktop() {
        let entities = vec![
            TextEntity {
                kind: TextEntityKind::Text,
                text: "Hello ".to_string(),
                extra: json!({}),
            },
            TextEntity {
                kind: TextEntityKind::Bold,
                text: "family".to_string(),
                extra: json!({}),
            },
            TextEntity {
                kind: TextEntityKind::TextUrl,
                text: " link".to_string(),
                extra: json!({ "href": "https://example.com?a=1&b=2" }),
            },
            TextEntity {
                kind: TextEntityKind::Spoiler,
                text: " secret".to_string(),
                extra: json!({}),
            },
        ];

        assert_eq!(
            render_text_entities(&entities),
            "Hello <strong>family</strong><a href=\"https://example.com?a=1&amp;b=2\"> link</a><span class=\"spoiler hidden\" onclick=\"ShowSpoiler(this)\"><span aria-hidden=\"true\"> secret</span></span>"
        );
    }

    #[test]
    fn escapes_js_handler_arguments_before_html_attribute_escaping() {
        let entities = vec![
            TextEntity {
                kind: TextEntityKind::Hashtag,
                text: "#a\"\\b".to_string(),
                extra: json!({}),
            },
            TextEntity {
                kind: TextEntityKind::BotCommand,
                text: "/a\"\\b".to_string(),
                extra: json!({}),
            },
            TextEntity {
                kind: TextEntityKind::Cashtag,
                text: "$a\"\\b".to_string(),
                extra: json!({}),
            },
        ];

        let html = render_text_entities(&entities);

        assert!(html.contains(r#"onclick="return ShowHashtag(&quot;a\&quot;\\b&quot;)""#));
        assert!(html.contains(r#"onclick="return ShowBotCommand(&quot;a\&quot;\\b&quot;)""#));
        assert!(html.contains(r#"onclick="return ShowCashtag(&quot;a\&quot;\\b&quot;)""#));
    }

    #[test]
    fn escapes_js_handler_arguments_that_try_to_break_the_handler() {
        let entities = vec![TextEntity {
            kind: TextEntityKind::Hashtag,
            text: "#x\");alert(1)//".to_string(),
            extra: json!({}),
        }];

        let html = render_text_entities(&entities);

        assert!(html.contains(r#"onclick="return ShowHashtag(&quot;x\&quot;);alert(1)//&quot;)""#));
        assert!(!html.contains(r#"onclick="return ShowHashtag(&quot;x&quot;);alert(1)//&quot;)""#));
    }

    #[test]
    fn rejects_unsafe_url_hrefs() {
        let entities = vec![
            TextEntity {
                kind: TextEntityKind::TextUrl,
                text: "click <me>".to_string(),
                extra: json!({ "href": "javascript:alert(1)" }),
            },
            TextEntity {
                kind: TextEntityKind::Url,
                text: "javascript:alert(1)".to_string(),
                extra: json!({}),
            },
        ];

        let html = render_text_entities(&entities);

        assert_eq!(html, "click &lt;me&gt;javascript:alert(1)");
        assert!(!html.contains("href=\"javascript:"));
    }

    #[test]
    fn rejects_text_url_hrefs_with_newlines_without_rendering_br_in_href() {
        let entities = vec![TextEntity {
            kind: TextEntityKind::TextUrl,
            text: "newline".to_string(),
            extra: json!({ "href": "https://example.com/a\nb" }),
        }];

        let html = render_text_entities(&entities);

        assert_eq!(html, "newline");
        assert!(!html.contains("<br>"));
        assert!(!html.contains("href="));
    }

    #[test]
    fn formats_telegram_dates_and_times() {
        let date = parse_utc("2025-02-12T08:37:48Z").unwrap();

        assert_eq!(format_date_text(date), "12 February 2025");
        assert_eq!(format_time_text(date), "08:37");
        assert_eq!(format_title_timestamp(date), "12.02.2025 08:37:48 UTC");
    }

    #[test]
    fn wraps_chat_page_with_telegram_desktop_shell() {
        let html = render_page(
            "Family Chat",
            "<div class=\"message service\" id=\"message-1\"></div>",
        );

        assert!(html.starts_with("<!DOCTYPE html>\n<html>"));
        assert!(html.contains("<title>Exported Data</title>"));
        assert!(html.contains("<body onload=\"CheckLocation();\">"));
        assert!(html.contains("<div class=\"text bold\">Family Chat</div>"));
        assert!(html.contains("<div class=\"page_body chat_page\">"));
        assert!(html.ends_with("</html>\n"));
    }

    #[test]
    fn escapes_chat_title_while_leaving_history_html_raw() {
        let history_html = "<div class=\"message\">A&B<br></div>";
        let html = render_page("A&B <Chat>", history_html);

        assert!(html.contains("<div class=\"text bold\">A&amp;B &lt;Chat&gt;</div>"));
        assert!(html.contains(history_html));
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use crate::export_rows::{
        AttachmentRow, ExportRows, MessageRow, PollOptionRow, PollRow, ServiceEventRow, TimelineRow,
    };

    fn message_row(id: i64, timeline_item_id: i64, sender: &str, text: &str) -> MessageRow {
        MessageRow {
            timeline_item_id,
            telegram_message_id: id,
            sender_name: Some(sender.to_string()),
            sender_inferred: false,
            edited_timestamp: None,
            plain_text: Some(text.to_string()),
            text_entities_json: "[]".to_string(),
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

    fn timeline_message(id: i64, message_id: i64, sender: &str, text: &str) -> TimelineRow {
        TimelineRow {
            id,
            ordinal: 0,
            item_kind: "message".to_string(),
            source_anchor: Some(format!("message{message_id}")),
            telegram_message_id: Some(message_id),
            timestamp: Some("2025-02-12T13:00:00Z".to_string()),
            original_timestamp: None,
            actor_name: Some(sender.to_string()),
            display_text: Some(text.to_string()),
            extra_json: "{}".to_string(),
        }
    }

    fn file_attachment(
        timeline_item_id: i64,
        relative_path: Option<&str>,
        href: &str,
    ) -> AttachmentRow {
        AttachmentRow {
            timeline_item_id,
            attachment_kind: "file".to_string(),
            relative_path: relative_path.map(ToOwned::to_owned),
            thumbnail_path: None,
            mime_type: Some("application/pdf".to_string()),
            file_size: None,
            duration_seconds: None,
            title: Some("report.pdf".to_string()),
            width: None,
            height: None,
            spoiler: false,
            ttl_seconds: None,
            skip_reason: None,
            extra_json: format!(r#"{{"href":"{href}"}}"#),
        }
    }

    #[test]
    fn renders_invite_member_service_text_from_structured_fields() {
        let service_event = ServiceEventRow {
            timeline_item_id: 4,
            event_type: "invite_members".to_string(),
            actor_name: Some("Alice".to_string()),
            target_names_json: "[\"Bob\"]".to_string(),
            display_text: "Alice invite members: Bob".to_string(),
            extra_json: "{}".to_string(),
        };

        assert_eq!(
            service_event_display_text(&service_event),
            "Alice invited Bob"
        );

        let mut malformed_targets = service_event;
        malformed_targets.target_names_json = "not json".to_string();

        assert_eq!(
            service_event_display_text(&malformed_targets),
            "Alice invite members: Bob"
        );
    }

    #[test]
    fn renders_parser_recognized_service_text_from_structured_fields() {
        let cases = [
            (
                ServiceEventRow {
                    timeline_item_id: 1,
                    event_type: "remove_members".to_string(),
                    actor_name: Some("Alice".to_string()),
                    target_names_json: "[\"Bob\"]".to_string(),
                    display_text: "Alice remove members: Bob".to_string(),
                    extra_json: "{}".to_string(),
                },
                "Alice removed Bob",
            ),
            (
                ServiceEventRow {
                    timeline_item_id: 2,
                    event_type: "edit_group_title".to_string(),
                    actor_name: Some("Alice".to_string()),
                    target_names_json: "[]".to_string(),
                    display_text: "Alice edit group title".to_string(),
                    extra_json: r#"{"source_json":{"title":"Family Archive"}}"#.to_string(),
                },
                "Alice changed group title to «Family Archive»",
            ),
            (
                ServiceEventRow {
                    timeline_item_id: 3,
                    event_type: "migrate_from_group".to_string(),
                    actor_name: Some("Family Archive".to_string()),
                    target_names_json: "[]".to_string(),
                    display_text: "Family Archive migrate from group".to_string(),
                    extra_json: r#"{"source_json":{"title":"Family Archive"}}"#.to_string(),
                },
                "Family Archive converted a basic group to this supergroup «Family Archive»",
            ),
            (
                ServiceEventRow {
                    timeline_item_id: 4,
                    event_type: "pin_message".to_string(),
                    actor_name: Some("Alice".to_string()),
                    target_names_json: "[]".to_string(),
                    display_text: "Alice pin message".to_string(),
                    extra_json: "{}".to_string(),
                },
                "Alice pinned this message",
            ),
            (
                ServiceEventRow {
                    timeline_item_id: 5,
                    event_type: "group_call".to_string(),
                    actor_name: Some("Alice".to_string()),
                    target_names_json: "[]".to_string(),
                    display_text: "Alice group call".to_string(),
                    extra_json: "{}".to_string(),
                },
                "Alice started voice chat",
            ),
        ];

        for (service_event, expected) in cases {
            assert_eq!(service_event_display_text(&service_event), expected);
        }

        let actorless_call = ServiceEventRow {
            timeline_item_id: 6,
            event_type: "group_call".to_string(),
            actor_name: None,
            target_names_json: "[]".to_string(),
            display_text: "group call".to_string(),
            extra_json: "{}".to_string(),
        };

        assert_eq!(service_event_display_text(&actorless_call), "Voice chat");
    }

    #[test]
    fn renders_messages_with_date_separator_and_joined_grouping() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![
                TimelineRow {
                    id: 1,
                    ordinal: 0,
                    item_kind: "message".to_string(),
                    source_anchor: Some("message101".to_string()),
                    telegram_message_id: Some(101),
                    timestamp: Some("2025-02-12T08:37:48Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Hello".to_string()),
                    extra_json: "{}".to_string(),
                },
                TimelineRow {
                    id: 2,
                    ordinal: 1,
                    item_kind: "message".to_string(),
                    source_anchor: Some("message102".to_string()),
                    telegram_message_id: Some(102),
                    timestamp: Some("2025-02-12T08:38:01Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Second".to_string()),
                    extra_json: "{}".to_string(),
                },
            ],
            messages: vec![
                message_row(101, 1, "Alice", "Hello"),
                message_row(102, 2, "Alice", "Second"),
            ],
            service_events: vec![],
            attachments: vec![],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert_eq!(rendered.generated_date_separators, 1);
        assert!(rendered.html.contains("id=\"message-1\""));
        assert!(rendered.html.contains("12 February 2025"));
        assert!(rendered.html.contains("class=\"message default clearfix\""));
        assert!(
            rendered
                .html
                .contains("class=\"message default clearfix joined\"")
        );
        assert!(rendered.html.contains("id=\"message101\""));
        assert!(rendered.html.contains("id=\"message102\""));
    }

    #[test]
    fn stored_date_separator_resets_joined_grouping_without_rendering() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![
                TimelineRow {
                    id: 1,
                    ordinal: 0,
                    item_kind: "message".to_string(),
                    source_anchor: Some("message101".to_string()),
                    telegram_message_id: Some(101),
                    timestamp: Some("2025-02-12T08:37:48Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Hello".to_string()),
                    extra_json: "{}".to_string(),
                },
                TimelineRow {
                    id: 2,
                    ordinal: 1,
                    item_kind: "date_separator".to_string(),
                    source_anchor: None,
                    telegram_message_id: None,
                    timestamp: Some("2025-02-12T00:00:00Z".to_string()),
                    original_timestamp: None,
                    actor_name: None,
                    display_text: Some("12 February 2025".to_string()),
                    extra_json: "{}".to_string(),
                },
                TimelineRow {
                    id: 3,
                    ordinal: 2,
                    item_kind: "message".to_string(),
                    source_anchor: Some("message102".to_string()),
                    telegram_message_id: Some(102),
                    timestamp: Some("2025-02-12T08:38:01Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Second".to_string()),
                    extra_json: "{}".to_string(),
                },
            ],
            messages: vec![
                message_row(101, 1, "Alice", "Hello"),
                message_row(102, 3, "Alice", "Second"),
            ],
            service_events: vec![],
            attachments: vec![],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert_eq!(rendered.generated_date_separators, 1);
        assert!(rendered.html.contains("id=\"message-1\""));
        assert!(!rendered.html.contains("id=\"message2\""));
        assert!(
            rendered
                .html
                .contains("<div class=\"message default clearfix\" id=\"message102\">")
        );
        assert!(
            !rendered
                .html
                .contains("<div class=\"message default clearfix joined\" id=\"message102\">")
        );
    }

    #[test]
    fn renders_reply_forward_media_poll_service_and_reactions() {
        let mut message = message_row(103, 3, "Bob", "Attached report");
        message.reply_to_message_id = Some(101);
        message.forwarded_from = Some("Carol".to_string());
        message.forwarded_date = Some("2025-02-11T22:00:00Z".to_string());
        message.reactions_json = r#"[{"emoji":"👍","count":2}]"#.to_string();
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![
                TimelineRow {
                    id: 3,
                    ordinal: 0,
                    item_kind: "message".to_string(),
                    source_anchor: Some("message103".to_string()),
                    telegram_message_id: Some(103),
                    timestamp: Some("2025-02-12T09:00:00Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Bob".to_string()),
                    display_text: Some("Attached report".to_string()),
                    extra_json: "{}".to_string(),
                },
                TimelineRow {
                    id: 4,
                    ordinal: 1,
                    item_kind: "service_event".to_string(),
                    source_anchor: Some("message104".to_string()),
                    telegram_message_id: Some(104),
                    timestamp: Some("2025-02-12T09:05:00Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Alice invited Bob".to_string()),
                    extra_json: "{}".to_string(),
                },
            ],
            messages: vec![message],
            service_events: vec![ServiceEventRow {
                timeline_item_id: 4,
                event_type: "invite_members".to_string(),
                actor_name: Some("Alice".to_string()),
                target_names_json: "[\"Bob\"]".to_string(),
                display_text: "Alice invited Bob".to_string(),
                extra_json: "{}".to_string(),
            }],
            attachments: vec![AttachmentRow {
                timeline_item_id: 3,
                attachment_kind: "file".to_string(),
                relative_path: Some("files/report.pdf".to_string()),
                thumbnail_path: None,
                mime_type: Some("application/pdf".to_string()),
                file_size: Some(12_288),
                duration_seconds: None,
                title: Some("report.pdf".to_string()),
                width: None,
                height: None,
                spoiler: false,
                ttl_seconds: None,
                skip_reason: None,
                extra_json: "{}".to_string(),
            }],
            polls: vec![PollRow {
                id: 1,
                timeline_item_id: 3,
                question: "Lunch?".to_string(),
                closed: Some(false),
                total_voters: Some(3),
                extra_json: "{}".to_string(),
            }],
            poll_options: vec![PollOptionRow {
                poll_id: 1,
                option_index: 0,
                text: "Pizza".to_string(),
                voters: Some(2),
                chosen: Some(true),
                extra_json: "{}".to_string(),
            }],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(rendered.html.contains("In reply to <a href=\"#go_to_message101\" onclick=\"return GoToMessage(101)\">this message</a>"));
        assert!(rendered.html.contains("class=\"forwarded body\""));
        assert!(rendered.html.contains("class=\"from_name\""));
        assert!(
            rendered
                .html
                .contains("class=\"date details\" title=\"11.02.2025 22:00:00 UTC\"")
        );
        assert!(rendered.html.contains("<div class=\"body\">"));
        assert!(rendered.html.contains("class=\"media_wrap clearfix\""));
        assert!(rendered.html.contains("href=\"files/report.pdf\""));
        assert!(
            rendered
                .html
                .contains("class=\"media clearfix pull_left block_link media_file\"")
        );
        assert!(rendered.html.contains("class=\"media_poll\""));
        assert!(
            rendered
                .html
                .contains("- Pizza <span class=\"details\">2 votes, chosen vote</span>")
        );
        assert!(
            rendered
                .html
                .contains("class=\"message service\" id=\"message104\"")
        );
        assert!(rendered.html.contains("<span class=\"emoji\">👍</span>"));
        assert!(rendered.html.contains("<span class=\"count\">2</span>"));
    }

    #[test]
    fn file_attachment_paths_reject_protocol_relative_and_root_absolute_hrefs() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![TimelineRow {
                id: 8,
                ordinal: 0,
                item_kind: "message".to_string(),
                source_anchor: Some("message108".to_string()),
                telegram_message_id: Some(108),
                timestamp: Some("2025-02-12T13:00:00Z".to_string()),
                original_timestamp: None,
                actor_name: Some("Grace".to_string()),
                display_text: Some("Files".to_string()),
                extra_json: "{}".to_string(),
            }],
            messages: vec![message_row(108, 8, "Grace", "Files")],
            service_events: vec![],
            attachments: vec![
                AttachmentRow {
                    timeline_item_id: 8,
                    attachment_kind: "file".to_string(),
                    relative_path: Some("//host/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("remote.png".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
                AttachmentRow {
                    timeline_item_id: 8,
                    attachment_kind: "file".to_string(),
                    relative_path: Some("/absolute/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("absolute.png".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
                AttachmentRow {
                    timeline_item_id: 8,
                    attachment_kind: "file".to_string(),
                    relative_path: Some("https://example.test/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("scheme.png".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
                AttachmentRow {
                    timeline_item_id: 8,
                    attachment_kind: "file".to_string(),
                    relative_path: Some("files/report.pdf".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("application/pdf".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("report.pdf".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
            ],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(!rendered.html.contains("href=\"//host/file.png\""));
        assert!(!rendered.html.contains("href=\"/absolute/file.png\""));
        assert!(
            !rendered
                .html
                .contains("href=\"https://example.test/file.png\"")
        );
        assert!(rendered.html.contains("href=\"files/report.pdf\""));
    }

    #[test]
    fn unsafe_original_href_parent_traversal_falls_back_to_safe_normalized_path() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![timeline_message(10, 110, "Grace", "Files")],
            messages: vec![message_row(110, 10, "Grace", "Files")],
            service_events: vec![],
            attachments: vec![
                file_attachment(10, Some("files/report.pdf"), "../outside.pdf"),
                file_attachment(10, Some("photos/subdir/photo.jpg"), "..\\\\outside.pdf"),
            ],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(!rendered.html.contains("href=\"../outside.pdf\""));
        assert!(!rendered.html.contains("href=\"..\\outside.pdf\""));
        assert!(rendered.html.contains("href=\"files/report.pdf\""));
        assert!(rendered.html.contains("href=\"photos/subdir/photo.jpg\""));
    }

    #[test]
    fn percent_encoded_original_href_parent_traversal_falls_back_to_safe_normalized_path() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![timeline_message(12, 112, "Grace", "Files")],
            messages: vec![message_row(112, 12, "Grace", "Files")],
            service_events: vec![],
            attachments: vec![
                file_attachment(12, Some("files/report.pdf"), "%2e%2e/outside.pdf"),
                file_attachment(12, Some("photos/photo_1.jpg"), ".%2e/outside.pdf"),
                file_attachment(12, Some("photos/subdir/photo.jpg"), "%2e./outside.pdf"),
            ],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(!rendered.html.contains("href=\"%2e%2e/outside.pdf\""));
        assert!(!rendered.html.contains("href=\".%2e/outside.pdf\""));
        assert!(!rendered.html.contains("href=\"%2e./outside.pdf\""));
        assert!(rendered.html.contains("href=\"files/report.pdf\""));
        assert!(rendered.html.contains("href=\"photos/photo_1.jpg\""));
        assert!(rendered.html.contains("href=\"photos/subdir/photo.jpg\""));
    }

    #[test]
    fn unsafe_original_href_url_and_absolute_forms_are_rejected_without_fallback() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![timeline_message(11, 111, "Grace", "Files")],
            messages: vec![message_row(111, 11, "Grace", "Files")],
            service_events: vec![],
            attachments: vec![
                file_attachment(11, None, "https://example.test/file.pdf"),
                file_attachment(11, None, "/absolute/file.pdf"),
                file_attachment(11, None, "//example.test/file.pdf"),
            ],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(
            !rendered
                .html
                .contains("href=\"https://example.test/file.pdf\"")
        );
        assert!(!rendered.html.contains("href=\"/absolute/file.pdf\""));
        assert!(!rendered.html.contains("href=\"//example.test/file.pdf\""));
    }

    #[test]
    fn photo_attachment_paths_reject_protocol_relative_and_root_absolute_href_src() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![TimelineRow {
                id: 9,
                ordinal: 0,
                item_kind: "message".to_string(),
                source_anchor: Some("message109".to_string()),
                telegram_message_id: Some(109),
                timestamp: Some("2025-02-12T14:00:00Z".to_string()),
                original_timestamp: None,
                actor_name: Some("Heidi".to_string()),
                display_text: Some("Photos".to_string()),
                extra_json: "{}".to_string(),
            }],
            messages: vec![message_row(109, 9, "Heidi", "Photos")],
            service_events: vec![],
            attachments: vec![
                AttachmentRow {
                    timeline_item_id: 9,
                    attachment_kind: "photo".to_string(),
                    relative_path: Some("//host/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("remote photo".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
                AttachmentRow {
                    timeline_item_id: 9,
                    attachment_kind: "photo".to_string(),
                    relative_path: Some("/absolute/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("absolute photo".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
                AttachmentRow {
                    timeline_item_id: 9,
                    attachment_kind: "photo".to_string(),
                    relative_path: Some("https://example.test/file.png".to_string()),
                    thumbnail_path: None,
                    mime_type: Some("image/png".to_string()),
                    file_size: None,
                    duration_seconds: None,
                    title: Some("scheme photo".to_string()),
                    width: None,
                    height: None,
                    spoiler: false,
                    ttl_seconds: None,
                    skip_reason: None,
                    extra_json: "{}".to_string(),
                },
            ],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(!rendered.html.contains("href=\"//host/file.png\""));
        assert!(!rendered.html.contains("src=\"//host/file.png\""));
        assert!(!rendered.html.contains("href=\"/absolute/file.png\""));
        assert!(!rendered.html.contains("src=\"/absolute/file.png\""));
        assert!(
            !rendered
                .html
                .contains("href=\"https://example.test/file.png\"")
        );
        assert!(
            !rendered
                .html
                .contains("src=\"https://example.test/file.png\"")
        );
    }

    #[test]
    fn malformed_json_degrades_to_plain_text_and_skips_buttons() {
        let mut message = message_row(105, 5, "Dana", "Visible <plain>");
        message.text_entities_json = "{bad entities".to_string();
        message.inline_bot_buttons_json = "{bad buttons".to_string();
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![TimelineRow {
                id: 5,
                ordinal: 0,
                item_kind: "message".to_string(),
                source_anchor: Some("message105".to_string()),
                telegram_message_id: Some(105),
                timestamp: Some("2025-02-12T10:00:00Z".to_string()),
                original_timestamp: None,
                actor_name: Some("Dana".to_string()),
                display_text: Some("Visible <plain>".to_string()),
                extra_json: "{}".to_string(),
            }],
            messages: vec![message],
            service_events: vec![],
            attachments: vec![],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(rendered.html.contains("Visible &lt;plain&gt;"));
        assert!(!rendered.html.contains("bot_buttons_table"));
    }

    #[test]
    fn closed_poll_and_zero_votes_render_for_round_trip() {
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![TimelineRow {
                id: 6,
                ordinal: 0,
                item_kind: "message".to_string(),
                source_anchor: Some("message106".to_string()),
                telegram_message_id: Some(106),
                timestamp: Some("2025-02-12T11:00:00Z".to_string()),
                original_timestamp: None,
                actor_name: Some("Eve".to_string()),
                display_text: Some("Poll".to_string()),
                extra_json: "{}".to_string(),
            }],
            messages: vec![message_row(106, 6, "Eve", "Poll")],
            service_events: vec![],
            attachments: vec![],
            polls: vec![PollRow {
                id: 2,
                timeline_item_id: 6,
                question: "Closed?".to_string(),
                closed: Some(true),
                total_voters: Some(0),
                extra_json: "{}".to_string(),
            }],
            poll_options: vec![PollOptionRow {
                poll_id: 2,
                option_index: 0,
                text: "No".to_string(),
                voters: Some(0),
                chosen: Some(false),
                extra_json: "{}".to_string(),
            }],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(rendered.html.contains("Closed poll"));
        assert!(
            rendered
                .html
                .contains("- No <span class=\"details\">0 votes</span>")
        );
        assert!(
            rendered
                .html
                .contains("<div class=\"total details\">0 votes total</div>")
        );
    }

    #[test]
    fn inline_buttons_use_bot_buttons_table_class() {
        let mut message = message_row(107, 7, "Frank", "Choose");
        message.inline_bot_buttons_json =
            r#"[{"text":"Open","url":"https://example.com"}]"#.to_string();
        let rows = ExportRows {
            chat_title: "Family Chat".to_string(),
            timeline_items: vec![TimelineRow {
                id: 7,
                ordinal: 0,
                item_kind: "message".to_string(),
                source_anchor: Some("message107".to_string()),
                telegram_message_id: Some(107),
                timestamp: Some("2025-02-12T12:00:00Z".to_string()),
                original_timestamp: None,
                actor_name: Some("Frank".to_string()),
                display_text: Some("Choose".to_string()),
                extra_json: "{}".to_string(),
            }],
            messages: vec![message],
            service_events: vec![],
            attachments: vec![],
            polls: vec![],
            poll_options: vec![],
        };

        let rendered = render_history(&rows).unwrap();

        assert!(rendered.html.contains("class=\"bot_buttons_table\""));
        assert!(rendered.html.contains("class=\"bot_button\""));
        assert!(rendered.html.contains(">Open<"));
        assert!(!rendered.html.contains("class=\"button\""));
        assert!(!rendered.html.contains("inline_bot_buttons"));
    }
}
