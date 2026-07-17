use crate::{
    error::{Result, TelegramExportError},
    model::*,
};
use chrono::{NaiveDateTime, TimeZone, Utc};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

struct DialogRef<'a> {
    index: usize,
    value: &'a Value,
}

/// Label for an account with no name in a Telegram export: a deleted account, or
/// a member/sender the export could not resolve. tdesktop's JSON writer emits
/// bare `null` for these (only its HTML writer uses this literal), so we normalize
/// both a null message `from` and a null service `members` entry to it, keeping
/// the row rather than dropping it.
const DELETED_ACCOUNT_NAME: &str = "Deleted Account";

pub fn parse_json_export_file(
    export_root: &Path,
    absolute_path: &Path,
    relative_path: &Path,
    source_file_parse_order: usize,
    starting_ordinal: i64,
) -> Result<ParsedExport> {
    let source = std::fs::read_to_string(absolute_path)?;
    let root: Value = serde_json::from_str(&source)?;
    let dialogs = export_dialogs(&root)?;
    let chat_title = export_chat_title(&dialogs);
    let mut parsed = ParsedExport {
        chat: Some(Chat { title: chat_title }),
        source_checksum: crate::discovery::sha256_hex(source.as_bytes()),
        ..Default::default()
    };
    let mut next_ordinal = starting_ordinal;

    for dialog in dialogs {
        let dialog_metadata = dialog_metadata(dialog.value);
        let messages = dialog
            .value
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TelegramExportError::Parse(
                    "JSON dialog does not contain messages array".to_string(),
                )
            })?;

        for (message_index, message) in messages.iter().enumerate() {
            parse_json_message(
                &mut parsed,
                export_root,
                relative_path,
                source_file_parse_order,
                next_ordinal,
                dialog.index,
                message_index,
                &dialog_metadata,
                message,
            );
            next_ordinal += 1;
        }
    }

    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
fn parse_json_message(
    parsed: &mut ParsedExport,
    export_root: &Path,
    relative_path: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    dialog_index: usize,
    message_index: usize,
    dialog_metadata: &Value,
    message: &Value,
) {
    let message_type = string_field(message, "type").unwrap_or("message");
    let telegram_message_id = i64_field(message, "id");
    let source_anchor = Some(source_anchor(
        dialog_index,
        message_index,
        telegram_message_id,
    ));
    let timestamp = timestamp_field(
        message,
        "date_unixtime",
        "date",
        &mut parsed.warnings,
        source_file_parse_order,
        ordinal,
    );
    let original_timestamp = string_field(message, "date").map(ToOwned::to_owned);
    let source_json = message.clone();

    match message_type {
        "message" => parse_regular_message(
            parsed,
            export_root,
            relative_path,
            source_file_parse_order,
            ordinal,
            source_anchor,
            telegram_message_id,
            timestamp,
            original_timestamp,
            dialog_metadata,
            &source_json,
        ),
        "service" => parse_service_message(
            parsed,
            export_root,
            source_file_parse_order,
            ordinal,
            source_anchor,
            telegram_message_id,
            timestamp,
            original_timestamp,
            dialog_metadata,
            &source_json,
        ),
        _ => parse_unsupported_message(
            parsed,
            source_file_parse_order,
            ordinal,
            source_anchor,
            telegram_message_id,
            timestamp,
            original_timestamp,
            dialog_metadata,
            &source_json,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_regular_message(
    parsed: &mut ParsedExport,
    export_root: &Path,
    relative_path: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    source_anchor: Option<String>,
    telegram_message_id: Option<i64>,
    timestamp: Option<String>,
    original_timestamp: Option<String>,
    dialog_metadata: &Value,
    source_json: &Value,
) {
    let raw_sender = string_field(source_json, "from").map(ToOwned::to_owned);
    let sender_id = string_or_number_field(source_json, "from_id");
    // A deleted account serializes as "from": null with "from_id" still present;
    // Telegram labels it "Deleted Account". Keeping the id lets distinct deleted
    // accounts stay distinct instead of collapsing into one anonymous sender.
    let sender_name = match (raw_sender, sender_id.is_some()) {
        (None, true) => Some(DELETED_ACCOUNT_NAME.to_string()),
        (raw, _) => raw,
    };
    let plain_text = source_json
        .get("text")
        .and_then(text_from_json)
        .filter(|text| !text.is_empty());
    let text_entities = text_entities_from_message(source_json);
    let edited_timestamp = timestamp_field(
        source_json,
        "edited_unixtime",
        "edited",
        &mut parsed.warnings,
        source_file_parse_order,
        ordinal,
    );

    parsed.timeline_items.push(TimelineItem {
        id: None,
        source_file_parse_order,
        source_anchor,
        telegram_message_id,
        ordinal,
        kind: TimelineItemKind::Message,
        timestamp: timestamp.clone(),
        original_timestamp: original_timestamp.clone(),
        actor_name: sender_name.clone(),
        actor_id: sender_id.clone(),
        display_text: plain_text.clone(),
        extra_json: json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
            "source_file": relative_path,
        }),
    });

    parsed.messages.push(Message {
        timeline_ordinal: ordinal,
        telegram_message_id,
        sender_name,
        sender_id,
        sender_inferred: false,
        edited_timestamp,
        plain_text,
        text_entities,
        reply_to_message_id: i64_field(source_json, "reply_to_message_id"),
        reply_to_peer_id: string_or_number_field(source_json, "reply_to_peer_id"),
        forwarded_from: string_field(source_json, "forwarded_from").map(ToOwned::to_owned),
        forwarded_from_id: string_or_number_field(source_json, "forwarded_from_id"),
        forwarded_date: timestamp_field(
            source_json,
            "forwarded_date_unixtime",
            "forwarded_date",
            &mut parsed.warnings,
            source_file_parse_order,
            ordinal,
        ),
        saved_from: string_field(source_json, "saved_from").map(ToOwned::to_owned),
        via_bot: string_field(source_json, "via_bot").map(ToOwned::to_owned),
        author: string_field(source_json, "author").map(ToOwned::to_owned),
        inline_bot_buttons: source_json
            .get("inline_bot_buttons")
            .cloned()
            .unwrap_or_else(|| json!([])),
        reactions: source_json
            .get("reactions")
            .cloned()
            .unwrap_or_else(|| json!([])),
        extra_json: json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
            "sender": sender_metadata(source_json),
        }),
    });

    parse_attachments(
        parsed,
        export_root,
        source_file_parse_order,
        ordinal,
        source_json,
    );
    parse_poll(parsed, ordinal, source_json);
}

#[allow(clippy::too_many_arguments)]
fn parse_service_message(
    parsed: &mut ParsedExport,
    export_root: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    source_anchor: Option<String>,
    telegram_message_id: Option<i64>,
    timestamp: Option<String>,
    original_timestamp: Option<String>,
    dialog_metadata: &Value,
    source_json: &Value,
) {
    let event_type = string_field(source_json, "action")
        .unwrap_or("unknown_json_service")
        .to_string();
    let actor_id = string_or_number_field(source_json, "actor_id");
    // A deleted/unresolved actor serializes as `"actor": null` with `"actor_id"`
    // still present (tdesktop pushes both together, wrapping the empty peer name
    // as null). Mirror the message-sender path: label it "Deleted Account" rather
    // than dropping the actor. The raw null stays preserved in extra_json.
    let actor_name = match (
        string_field(source_json, "actor").map(ToOwned::to_owned),
        actor_id.is_some(),
    ) {
        (None, true) => Some(DELETED_ACCOUNT_NAME.to_string()),
        (raw, _) => raw,
    };
    let target_names = service_member_names(source_json, "members");
    let display_text = service_display_text(actor_name.as_deref(), &event_type, &target_names);

    parsed.timeline_items.push(TimelineItem {
        id: None,
        source_file_parse_order,
        source_anchor,
        telegram_message_id,
        ordinal,
        kind: TimelineItemKind::ServiceEvent,
        timestamp,
        original_timestamp,
        actor_name: actor_name.clone(),
        actor_id,
        display_text: Some(display_text.clone()),
        extra_json: json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
        }),
    });
    parsed.service_events.push(ServiceEvent {
        timeline_ordinal: ordinal,
        event_type,
        actor_name,
        target_names,
        display_text,
        extra_json: json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
            "actor": actor_metadata(source_json),
        }),
    });

    // Service actions can carry media (e.g. edit_group_photo's `photo`); register it so
    // it is existence-checked, bundle-copied, and rendered like any other attachment.
    parse_attachments(
        parsed,
        export_root,
        source_file_parse_order,
        ordinal,
        source_json,
    );
}

#[allow(clippy::too_many_arguments)]
fn parse_unsupported_message(
    parsed: &mut ParsedExport,
    source_file_parse_order: usize,
    ordinal: i64,
    source_anchor: Option<String>,
    telegram_message_id: Option<i64>,
    timestamp: Option<String>,
    original_timestamp: Option<String>,
    dialog_metadata: &Value,
    source_json: &Value,
) {
    let message_type = string_field(source_json, "type").unwrap_or("unknown");
    let display_text = source_json
        .get("text")
        .and_then(text_from_json)
        .filter(|text| !text.is_empty())
        .or_else(|| Some(format!("unsupported JSON message type: {message_type}")));

    parsed.timeline_items.push(TimelineItem {
        id: None,
        source_file_parse_order,
        source_anchor,
        telegram_message_id,
        ordinal,
        kind: TimelineItemKind::Unsupported,
        timestamp,
        original_timestamp,
        actor_name: string_field(source_json, "from")
            .or_else(|| string_field(source_json, "actor"))
            .map(ToOwned::to_owned),
        actor_id: string_or_number_field(source_json, "from_id")
            .or_else(|| string_or_number_field(source_json, "actor_id")),
        display_text,
        extra_json: json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
            "unsupported_json_type": message_type,
        }),
    });
    push_warning(
        &mut parsed.warnings,
        source_file_parse_order,
        Some(ordinal),
        WarningCode::ExtraJsonOnly,
        "unsupported JSON message preserved in extra_json",
        json!({
            "source_json": source_json,
            "dialog": dialog_metadata,
            "unsupported_json_type": message_type,
        }),
    );
}

fn parse_attachments(
    parsed: &mut ParsedExport,
    export_root: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    source_json: &Value,
) {
    if let Some(photo_path) = string_field(source_json, "photo") {
        let attachment = path_attachment(
            "photo",
            photo_path,
            export_root,
            source_file_parse_order,
            ordinal,
            source_json,
            AttachmentFields {
                thumbnail_key: Some("thumbnail"),
                file_size_key: Some("photo_file_size"),
                title_key: None,
            },
            &mut parsed.warnings,
        );
        parsed.attachments.push(attachment);
    }

    if let Some(file_path) = string_field(source_json, "file") {
        let kind = string_field(source_json, "media_type").unwrap_or("file");
        let attachment = path_attachment(
            kind,
            file_path,
            export_root,
            source_file_parse_order,
            ordinal,
            source_json,
            AttachmentFields {
                thumbnail_key: Some("thumbnail"),
                file_size_key: Some("file_size"),
                title_key: Some("file_name"),
            },
            &mut parsed.warnings,
        );
        parsed.attachments.push(attachment);
    }

    if let Some(vcard) = string_field(source_json, "contact_vcard") {
        // A shared contact's vCard file. `contact_vcard` is a real relative path
        // (e.g. chats/chat_1/contacts/contact_1.vcard) under default export
        // settings, or a "(File not included…)" placeholder when media is excluded.
        // Route it through path_attachment so a real path is existence-checked and
        // bundle-copied and a placeholder degrades to a skip_reason, instead of
        // being dropped as pathless metadata (C24).
        let attachment = path_attachment(
            "contact_information",
            vcard,
            export_root,
            source_file_parse_order,
            ordinal,
            source_json,
            AttachmentFields {
                thumbnail_key: None,
                file_size_key: Some("contact_vcard_file_size"),
                title_key: None,
            },
            &mut parsed.warnings,
        );
        parsed.attachments.push(attachment);
    } else if source_json.get("contact_information").is_some() {
        // A contact with no vCard bytes: metadata only, nothing to copy.
        parsed.attachments.push(metadata_attachment(
            ordinal,
            "contact_information",
            source_json,
        ));
    }

    if source_json.get("location_information").is_some() {
        parsed.attachments.push(metadata_attachment(
            ordinal,
            "location_information",
            source_json,
        ));
    }
}

struct AttachmentFields {
    thumbnail_key: Option<&'static str>,
    file_size_key: Option<&'static str>,
    title_key: Option<&'static str>,
}

#[allow(clippy::too_many_arguments)]
fn path_attachment(
    kind: &str,
    path: &str,
    export_root: &Path,
    source_file_parse_order: usize,
    ordinal: i64,
    source_json: &Value,
    fields: AttachmentFields,
    warnings: &mut Vec<ImportWarning>,
) -> Attachment {
    let (relative_path, skip_reason) = attachment_path(path);
    if let Some(relative_path) = &relative_path
        && !export_root.join(relative_path).is_file()
    {
        push_warning(
            warnings,
            source_file_parse_order,
            Some(ordinal),
            WarningCode::MissingAttachment,
            "referenced JSON export attachment is missing",
            json!({
                "path": relative_path,
                "source_json": source_json,
            }),
        );
    }

    Attachment {
        timeline_ordinal: ordinal,
        kind: kind.to_string(),
        relative_path,
        thumbnail_path: fields
            .thumbnail_key
            .and_then(|key| string_field(source_json, key))
            .and_then(|path| attachment_path(path).0),
        mime_type: string_field(source_json, "mime_type").map(ToOwned::to_owned),
        file_size: fields
            .file_size_key
            .and_then(|key| u64_field(source_json, key)),
        duration_seconds: i64_field(source_json, "duration_seconds"),
        title: fields
            .title_key
            .and_then(|key| string_field(source_json, key))
            .map(ToOwned::to_owned),
        width: i64_field(source_json, "width"),
        height: i64_field(source_json, "height"),
        spoiler: bool_field(source_json, "media_spoiler").unwrap_or(false),
        ttl_seconds: i64_field(source_json, "self_destruct_period_seconds"),
        skip_reason,
        extra_json: json!({
            "source_json": source_json,
        }),
    }
}

fn metadata_attachment(ordinal: i64, kind: &str, source_json: &Value) -> Attachment {
    Attachment {
        timeline_ordinal: ordinal,
        kind: kind.to_string(),
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
        extra_json: json!({
            "source_json": source_json,
        }),
    }
}

fn attachment_path(path: &str) -> (Option<PathBuf>, Option<String>) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    if trimmed.starts_with('(') {
        return (None, Some(trimmed.to_string()));
    }
    (Some(PathBuf::from(trimmed)), None)
}

fn parse_poll(parsed: &mut ParsedExport, ordinal: i64, source_json: &Value) {
    let Some(poll) = source_json.get("poll") else {
        return;
    };
    let question = poll
        .get("question")
        .and_then(text_from_json)
        .filter(|question| !question.is_empty())
        .unwrap_or_else(|| "Unknown poll question".to_string());

    parsed.polls.push(Poll {
        timeline_ordinal: ordinal,
        question,
        closed: bool_field(poll, "closed"),
        total_voters: i64_field(poll, "total_voters"),
        extra_json: json!({
            "source_json": poll,
        }),
    });

    if let Some(answers) = poll.get("answers").and_then(Value::as_array) {
        for (index, answer) in answers.iter().enumerate() {
            let text = answer
                .get("text")
                .and_then(text_from_json)
                .filter(|text| !text.is_empty())
                .unwrap_or_default();
            parsed.poll_options.push(PollOption {
                timeline_ordinal: ordinal,
                option_index: index as i64,
                text,
                voters: i64_field(answer, "voters"),
                chosen: bool_field(answer, "chosen"),
                extra_json: json!({
                    "source_json": answer,
                }),
            });
        }
    }
}

fn export_dialogs(root: &Value) -> Result<Vec<DialogRef<'_>>> {
    if root.get("messages").and_then(Value::as_array).is_some() {
        return Ok(vec![DialogRef {
            index: 0,
            value: root,
        }]);
    }

    let mut dialogs = Vec::new();
    collect_dialogs(root, "chats", &mut dialogs);
    collect_dialogs(root, "left_chats", &mut dialogs);

    if dialogs.is_empty() {
        return Err(TelegramExportError::Parse(
            "JSON export does not contain chats.list, left_chats.list, or messages".to_string(),
        ));
    }

    // A single-chat export is exactly one dialog (root `messages`, handled above,
    // or a one-element chats.list). More than one dialog means a full-account
    // export: flattening those into one chat collides message ids across chats
    // (C21) and mislabels the result (C53). Refuse cleanly instead.
    if dialogs.len() > 1 {
        return Err(TelegramExportError::MultiChatExportNotSupported {
            chats: dialogs.len(),
        });
    }

    Ok(dialogs)
}

fn collect_dialogs<'a>(root: &'a Value, key: &str, dialogs: &mut Vec<DialogRef<'a>>) {
    if let Some(list) = root
        .get(key)
        .and_then(|section| section.get("list"))
        .and_then(Value::as_array)
    {
        for dialog in list {
            if dialog.get("messages").and_then(Value::as_array).is_some() {
                dialogs.push(DialogRef {
                    index: dialogs.len(),
                    value: dialog,
                });
            }
        }
    }
}

fn export_chat_title(dialogs: &[DialogRef<'_>]) -> String {
    if dialogs.len() == 1 {
        return string_field(dialogs[0].value, "name")
            .unwrap_or("Telegram JSON Export")
            .to_string();
    }

    "Telegram JSON Export".to_string()
}

fn dialog_metadata(dialog: &Value) -> Value {
    let mut metadata = Map::new();
    for key in ["name", "type", "id"] {
        if let Some(value) = dialog.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(metadata)
}

fn source_anchor(
    dialog_index: usize,
    message_index: usize,
    telegram_message_id: Option<i64>,
) -> String {
    match telegram_message_id {
        Some(id) => format!("json:{dialog_index}:{id}"),
        None => format!("json:{dialog_index}:index:{message_index}"),
    }
}

fn text_from_json(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts.iter().filter_map(text_from_json).collect::<String>();
            Some(text)
        }
        Value::Object(object) => object.get("text").and_then(text_from_json),
        _ => None,
    }
}

fn text_entities_from_message(message: &Value) -> Vec<TextEntity> {
    let entities = message
        .get("text_entities")
        .and_then(Value::as_array)
        .or_else(|| message.get("text").and_then(Value::as_array));

    entities
        .into_iter()
        .flatten()
        .filter_map(text_entity_from_json)
        .collect()
}

fn text_entity_from_json(value: &Value) -> Option<TextEntity> {
    match value {
        Value::String(text) => Some(TextEntity {
            kind: TextEntityKind::Text,
            text: text.clone(),
            extra: json!({}),
        }),
        Value::Object(object) => {
            let text = object
                .get("text")
                .and_then(text_from_json)
                .unwrap_or_default();
            let raw_type = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("plain");
            let kind = text_entity_kind(raw_type);
            let mut extra = Map::new();
            for (key, value) in object {
                if key != "type" && key != "text" {
                    extra.insert(key.clone(), value.clone());
                }
            }
            if kind == TextEntityKind::Unknown {
                extra.insert("raw_type".to_string(), json!(raw_type));
            }

            Some(TextEntity {
                kind,
                text,
                extra: Value::Object(extra),
            })
        }
        _ => None,
    }
}

fn text_entity_kind(raw_type: &str) -> TextEntityKind {
    match raw_type {
        "plain" => TextEntityKind::Text,
        "mention" => TextEntityKind::Mention,
        "hashtag" => TextEntityKind::Hashtag,
        "bot_command" => TextEntityKind::BotCommand,
        "link" => TextEntityKind::Url,
        "email" => TextEntityKind::Email,
        "bold" => TextEntityKind::Bold,
        "italic" => TextEntityKind::Italic,
        "code" => TextEntityKind::Code,
        "pre" => TextEntityKind::Pre,
        "text_link" => TextEntityKind::TextUrl,
        "mention_name" => TextEntityKind::MentionName,
        "phone" => TextEntityKind::Phone,
        "cashtag" => TextEntityKind::Cashtag,
        "underline" => TextEntityKind::Underline,
        "strikethrough" => TextEntityKind::Strike,
        "blockquote" => TextEntityKind::Blockquote,
        "bank_card" => TextEntityKind::BankCard,
        "spoiler" => TextEntityKind::Spoiler,
        "custom_emoji" => TextEntityKind::CustomEmoji,
        _ => TextEntityKind::Unknown,
    }
}

fn timestamp_field(
    object: &Value,
    unix_key: &str,
    raw_key: &str,
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    ordinal: i64,
) -> Option<String> {
    if let Some(value) = object.get(unix_key) {
        if let Some(seconds) = value_as_i64(value)
            && let Some(timestamp) = Utc.timestamp_opt(seconds, 0).single()
        {
            return Some(timestamp.to_rfc3339());
        }
        push_warning(
            warnings,
            source_file_parse_order,
            Some(ordinal),
            WarningCode::MalformedTimestamp,
            "malformed JSON unix timestamp",
            json!({
                "field": unix_key,
                "value": value,
            }),
        );
    }

    // Fallback for older exports without `date_unixtime`: the naive `date` string carries
    // no offset, so treat it as UTC and store a valid RFC3339 value. Storing it raw would
    // make export-html abort (parse_utc rejects offset-less strings).
    let raw = string_field(object, raw_key)?;
    match NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        Ok(naive) => Some(Utc.from_utc_datetime(&naive).to_rfc3339()),
        Err(_) => {
            push_warning(
                warnings,
                source_file_parse_order,
                Some(ordinal),
                WarningCode::MalformedTimestamp,
                "unparseable JSON date fallback",
                json!({ "field": raw_key, "value": raw }),
            );
            None
        }
    }
}

fn service_display_text(
    actor_name: Option<&str>,
    event_type: &str,
    target_names: &[String],
) -> String {
    let action = event_type.replace('_', " ");
    match (actor_name, target_names.is_empty()) {
        (Some(actor), false) => format!("{actor} {action}: {}", target_names.join(", ")),
        (Some(actor), true) => format!("{actor} {action}"),
        (None, false) => format!("{action}: {}", target_names.join(", ")),
        (None, true) => action,
    }
}

fn sender_metadata(message: &Value) -> Value {
    let mut metadata = Map::new();
    if let Some(id) = message.get("from_id") {
        metadata.insert("id".to_string(), id.clone());
    }
    if let Some(name) = message.get("from") {
        metadata.insert("name".to_string(), name.clone());
    }
    Value::Object(metadata)
}

fn actor_metadata(message: &Value) -> Value {
    let mut metadata = Map::new();
    if let Some(id) = message.get("actor_id") {
        metadata.insert("id".to_string(), id.clone());
    }
    if let Some(name) = message.get("actor") {
        metadata.insert("name".to_string(), name.clone());
    }
    Value::Object(metadata)
}

/// Names for a service event's `members` array. Each element is a non-empty name
/// string or JSON `null` (a deleted or unresolved account — tdesktop's JSON writer
/// emits bare `null`, never a "Deleted Account" string, which is HTML-only).
/// Preserve a `null` as [`DELETED_ACCOUNT_NAME`] so the member is kept and counted
/// rather than silently dropped (C25).
fn service_member_names(object: &Value, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            Value::String(name) => Some(name.clone()),
            Value::Null => Some(DELETED_ACCOUNT_NAME.to_string()),
            _ => None,
        })
        .collect()
}

fn string_field<'a>(object: &'a Value, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn string_or_number_field(object: &Value, key: &str) -> Option<String> {
    let value = object.get(key)?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn i64_field(object: &Value, key: &str) -> Option<i64> {
    object.get(key).and_then(value_as_i64)
}

fn u64_field(object: &Value, key: &str) -> Option<u64> {
    object.get(key).and_then(value_as_u64)
}

fn bool_field(object: &Value, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.parse::<i64>().ok(),
        _ => None,
    }
}

fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
}

fn push_warning(
    warnings: &mut Vec<ImportWarning>,
    source_file_parse_order: usize,
    timeline_ordinal: Option<i64>,
    code: WarningCode,
    message: &str,
    context: Value,
) {
    warnings.push(ImportWarning {
        source_file_parse_order: Some(source_file_parse_order),
        timeline_ordinal,
        code,
        message: message.to_string(),
        context,
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::model::{TextEntityKind, TimelineItemKind, WarningCode};

    #[test]
    fn json_timestamp_fallback_without_unixtime_is_rfc3339() {
        // Older JSON exports lack `date_unixtime`; the naive `date` fallback must still be
        // stored as an RFC3339 value the exporters can parse (parse_utc), not raw.
        let object = json!({ "date": "2019-05-14T12:33:21" });
        let mut warnings = Vec::new();
        let stored = timestamp_field(&object, "date_unixtime", "date", &mut warnings, 0, 0)
            .expect("naive date preserved");

        let parsed = crate::time::parse_utc(&stored).expect("stored timestamp is RFC3339");
        assert_eq!(parsed.to_rfc3339(), "2019-05-14T12:33:21+00:00");
        assert!(warnings.is_empty());
    }

    #[test]
    fn distinguishes_same_name_senders_and_deleted_accounts_by_id() {
        let dir = tempdir().unwrap();
        let result = r#"{"chats":{"list":[{"name":"Group","type":"private_group","id":1,"messages":[
            {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Alex","from_id":"user111","text":"a"},
            {"id":2,"type":"message","date":"2020-01-01T00:01:00","date_unixtime":"1577836860","from":"Alex","from_id":"user222","text":"b"},
            {"id":3,"type":"message","date":"2020-01-01T00:02:00","date_unixtime":"1577836920","from":null,"from_id":"user333","text":"c"}
        ]}]}}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();
        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let by_text = |text: &str| {
            parsed
                .messages
                .iter()
                .find(|m| m.plain_text.as_deref() == Some(text))
                .unwrap()
        };
        assert_eq!(by_text("a").sender_id.as_deref(), Some("user111"));
        assert_eq!(by_text("b").sender_id.as_deref(), Some("user222"));
        assert_ne!(by_text("a").sender_id, by_text("b").sender_id);
        // Deleted account: "from": null but "from_id" present -> labeled yet distinct.
        assert_eq!(by_text("c").sender_name.as_deref(), Some("Deleted Account"));
        assert_eq!(by_text("c").sender_id.as_deref(), Some("user333"));
    }

    #[test]
    fn service_members_preserve_null_deleted_accounts() {
        // tdesktop serializes a deleted or otherwise unresolved member as bare
        // JSON null in `members`. Preserve it as "Deleted Account" (the same label
        // the message-sender path uses for a null `from`) so the member is kept and
        // counted, not silently dropped (C25).
        let dir = tempdir().unwrap();
        let result = r#"{"name":"Group","type":"private_group","id":1,"messages":[
            {"id":1,"type":"service","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","actor":"Alice","actor_id":"user1","action":"invite_members","members":["Bob",null,"Carol"]}
        ]}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();

        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let service = &parsed.service_events[0];
        assert_eq!(
            service.target_names,
            vec!["Bob", "Deleted Account", "Carol"]
        );
        assert!(
            service.display_text.contains("Bob, Deleted Account, Carol"),
            "display text keeps all three members in order: {}",
            service.display_text
        );
    }

    #[test]
    fn service_null_actor_is_labeled_deleted_account() {
        // A deleted/unresolved account performing a service action serializes as
        // `"actor": null` with `"actor_id"` still present: tdesktop's
        // pushFrom("actor") emits both fields together under one `if (fromId)`
        // guard and wraps the empty peer name as bare JSON null (StringAllowNull
        // in export_output_json.cpp). Mirror the message-sender path and label it
        // "Deleted Account" instead of dropping the actor to None, while leaving
        // the raw null untouched in extra_json.
        let dir = tempdir().unwrap();
        let result = r#"{"name":"Group","type":"private_group","id":1,"messages":[
            {"id":1,"type":"service","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","actor":null,"actor_id":5566,"action":"pin_message"}
        ]}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();

        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let service = &parsed.service_events[0];
        assert_eq!(service.actor_name.as_deref(), Some("Deleted Account"));
        assert!(
            service.display_text.starts_with("Deleted Account"),
            "display text is prefixed with the labeled actor: {}",
            service.display_text
        );
        // The timeline row's typed actor mirrors the service event.
        assert_eq!(
            parsed.timeline_items[0].actor_name.as_deref(),
            Some("Deleted Account")
        );
        // Fidelity: the raw null is preserved untouched in extra_json.
        assert!(service.extra_json["actor"]["name"].is_null());
        assert_eq!(service.extra_json["source_json"]["actor_id"], 5566);
    }

    #[test]
    fn service_message_media_is_registered_as_attachment() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("group_photo.jpg"), "img").unwrap();
        let result = r#"{"chats":{"list":[{"name":"Group","type":"private_group","id":1,"messages":[
            {"id":1,"type":"service","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","actor":"Alice","actor_id":"user1","action":"edit_group_photo","photo":"group_photo.jpg","width":640,"height":640}
        ]}]}}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();
        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let photo = parsed
            .attachments
            .iter()
            .find(|attachment| attachment.kind == "photo")
            .expect("service message photo registered as an attachment");
        assert_eq!(
            photo.relative_path.as_deref(),
            Some(Path::new("group_photo.jpg"))
        );
        assert_eq!(photo.timeline_ordinal, 0);
    }

    #[test]
    fn contact_vcard_is_registered_as_a_copyable_attachment() {
        // A shared contact's vCard is a real relative file path in JSON; it must
        // be recorded with that relative_path (so bundling copies the .vcard file)
        // and its file size, not dropped as pathless metadata (C24).
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("contacts")).unwrap();
        std::fs::write(dir.path().join("contacts/contact_1.vcard"), "BEGIN:VCARD").unwrap();
        let result = r#"{"name":"Chat","type":"personal_chat","id":1,"messages":[
            {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Alice","from_id":"user1",
             "contact_information":{"first_name":"Bob","last_name":"Jones","phone_number":"+1 234"},
             "contact_vcard":"contacts/contact_1.vcard","contact_vcard_file_size":11}
        ]}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();

        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let contact = parsed
            .attachments
            .iter()
            .find(|a| a.kind == "contact_information")
            .expect("contact registered as an attachment");
        assert_eq!(
            contact.relative_path.as_deref(),
            Some(Path::new("contacts/contact_1.vcard"))
        );
        assert_eq!(contact.file_size, Some(11));
        assert_eq!(contact.skip_reason, None);
        assert!(
            parsed.warnings.is_empty(),
            "the vcard file exists, so there is no missing-attachment warning"
        );
    }

    #[test]
    fn contact_vcard_placeholder_becomes_a_skip_reason_not_a_path() {
        // When media is excluded from the export, contact_vcard is a
        // "(File not included…)" placeholder; it must become a skip_reason with no
        // relative_path (nothing to copy), like other placeholder attachments.
        let dir = tempdir().unwrap();
        let result = r#"{"name":"Chat","type":"personal_chat","id":1,"messages":[
            {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Alice","from_id":"user1",
             "contact_information":{"first_name":"Bob"},
             "contact_vcard":"(File not included. Change data exporting settings to download.)"}
        ]}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();

        let parsed = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .unwrap();

        let contact = parsed
            .attachments
            .iter()
            .find(|a| a.kind == "contact_information")
            .expect("contact registered as an attachment");
        assert_eq!(contact.relative_path, None);
        assert_eq!(
            contact.skip_reason.as_deref(),
            Some("(File not included. Change data exporting settings to download.)")
        );
    }

    #[test]
    fn parses_representative_json_export() {
        let export_root = Path::new("tests/fixtures/json_export");
        let parsed = parse_json_export_file(
            export_root,
            &export_root.join("result.json"),
            Path::new("result.json"),
            0,
            10,
        )
        .unwrap();

        assert_eq!(parsed.chat.unwrap().title, "Family Chat");
        assert_eq!(parsed.timeline_items.len(), 4);
        assert_eq!(parsed.messages.len(), 3);
        assert_eq!(parsed.service_events.len(), 1);
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.polls.len(), 1);
        assert_eq!(parsed.poll_options.len(), 2);
        assert!(parsed.warnings.is_empty());

        let first_item = &parsed.timeline_items[0];
        assert_eq!(first_item.kind, TimelineItemKind::Message);
        assert_eq!(first_item.ordinal, 10);
        assert_eq!(first_item.telegram_message_id, Some(101));
        assert_eq!(
            first_item.timestamp.as_deref(),
            Some("2025-02-12T08:37:48+00:00")
        );
        assert_eq!(
            first_item.original_timestamp.as_deref(),
            Some("2025-02-12T08:37:48")
        );
        assert_eq!(
            first_item.display_text.as_deref(),
            Some("Hello family link")
        );
        assert_eq!(first_item.extra_json["source_json"]["from_id"], "user12345");

        let first_message = &parsed.messages[0];
        assert_eq!(first_message.telegram_message_id, Some(101));
        assert_eq!(first_message.sender_name.as_deref(), Some("Alice"));
        assert_eq!(
            first_message.plain_text.as_deref(),
            Some("Hello family link")
        );
        assert_eq!(
            first_message.edited_timestamp.as_deref(),
            Some("2025-02-12T08:40:00+00:00")
        );
        assert_eq!(first_message.reply_to_message_id, Some(100));
        assert_eq!(first_message.extra_json["sender"]["id"], "user12345");
        assert_eq!(
            first_message.inline_bot_buttons,
            json!([[{
                "type": "url",
                "text": "Open",
                "data": "https://example.com"
            }]])
        );
        assert_eq!(
            first_message.reactions,
            json!([{
                "type": "emoji",
                "count": 2,
                "emoji": "thumbs_up"
            }])
        );
        assert_eq!(first_message.text_entities.len(), 3);
        assert_eq!(first_message.text_entities[1].kind, TextEntityKind::Bold);
        assert_eq!(first_message.text_entities[2].kind, TextEntityKind::TextUrl);
        assert_eq!(
            first_message.text_entities[2].extra["href"],
            "https://example.com"
        );

        let service = &parsed.service_events[0];
        assert_eq!(service.timeline_ordinal, 11);
        assert_eq!(service.event_type, "invite_members");
        assert_eq!(service.actor_name.as_deref(), Some("Alice"));
        assert_eq!(service.target_names, vec!["Bob"]);
        assert_eq!(service.extra_json["source_json"]["actor_id"], "user12345");

        let attachment = &parsed.attachments[0];
        assert_eq!(attachment.timeline_ordinal, 12);
        assert_eq!(attachment.kind, "file");
        assert_eq!(
            attachment.relative_path,
            Some(PathBuf::from("files/report.pdf"))
        );
        assert_eq!(attachment.title.as_deref(), Some("report.pdf"));
        assert_eq!(attachment.file_size, Some(12));
        assert_eq!(attachment.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(
            attachment.extra_json["source_json"]["file"],
            "files/report.pdf"
        );

        assert_eq!(parsed.polls[0].timeline_ordinal, 13);
        assert_eq!(parsed.polls[0].question, "Lunch?");
        assert_eq!(parsed.polls[0].closed, Some(false));
        assert_eq!(parsed.polls[0].total_voters, Some(3));
        assert_eq!(parsed.poll_options[0].text, "Pizza");
        assert_eq!(parsed.poll_options[0].voters, Some(2));
        assert_eq!(parsed.poll_options[0].chosen, Some(true));
    }

    #[test]
    fn refuses_multi_chat_full_account_export() {
        // A full-account export nests multiple dialogs under chats.list (and
        // left_chats.list); a single-chat export is one dialog (root `messages` or
        // a one-element chats.list). This tool archives one chat per database, so a
        // multi-chat export must be refused cleanly rather than flattened into one
        // chat with colliding message ids (C21).
        let dir = tempdir().unwrap();
        let result = r#"{"chats":{"list":[
            {"name":"Alice","type":"personal_chat","id":111,"messages":[
                {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Alice","from_id":"user111","text":"a"}
            ]},
            {"name":"My Group","type":"private_group","id":222,"messages":[
                {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Bob","from_id":"user222","text":"b"}
            ]}
        ]}}"#;
        std::fs::write(dir.path().join("result.json"), result).unwrap();

        let error = parse_json_export_file(
            dir.path(),
            &dir.path().join("result.json"),
            Path::new("result.json"),
            0,
            0,
        )
        .expect_err("multi-chat export must be refused");
        assert!(matches!(
            error,
            TelegramExportError::MultiChatExportNotSupported { chats } if chats == 2
        ));
    }

    #[test]
    fn parses_single_peer_json_export() {
        let dir = tempdir().unwrap();
        let result = dir.path().join("result.json");
        std::fs::write(
            &result,
            r#"{
              "name": "Saved Messages",
              "type": "saved_messages",
              "id": 1,
              "messages": [
                {
                  "id": 1,
                  "type": "message",
                  "date": "2025-01-01T00:00:00",
                  "date_unixtime": "1735689600",
                  "from": "Me",
                  "from_id": "user1",
                  "text": "note"
                }
              ]
            }"#,
        )
        .unwrap();

        let parsed =
            parse_json_export_file(dir.path(), &result, Path::new("result.json"), 0, 0).unwrap();

        assert_eq!(parsed.chat.unwrap().title, "Saved Messages");
        assert_eq!(parsed.timeline_items.len(), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].plain_text.as_deref(), Some("note"));
        assert_eq!(
            parsed.timeline_items[0].extra_json["dialog"]["type"],
            "saved_messages"
        );
    }

    #[test]
    fn preserves_unknown_json_message_as_unsupported() {
        let dir = tempdir().unwrap();
        let result = dir.path().join("result.json");
        std::fs::write(
            &result,
            r#"{
              "name": "Future Chat",
              "type": "personal_chat",
              "id": 42,
              "messages": [
                {
                  "id": 999,
                  "type": "future",
                  "date": "2025-01-01T00:00:00",
                  "date_unixtime": "1735689600",
                  "future_payload": {
                    "shape": "not-yet-known"
                  }
                }
              ]
            }"#,
        )
        .unwrap();

        let parsed =
            parse_json_export_file(dir.path(), &result, Path::new("result.json"), 0, 0).unwrap();

        assert_eq!(parsed.timeline_items.len(), 1);
        assert_eq!(parsed.timeline_items[0].kind, TimelineItemKind::Unsupported);
        assert_eq!(
            parsed.timeline_items[0].extra_json["source_json"]["future_payload"]["shape"],
            "not-yet-known"
        );
        assert_eq!(parsed.warnings.len(), 1);
        assert_eq!(parsed.warnings[0].code, WarningCode::ExtraJsonOnly);
        assert_eq!(
            parsed.warnings[0].context["source_json"]["future_payload"]["shape"],
            "not-yet-known"
        );
    }
}
