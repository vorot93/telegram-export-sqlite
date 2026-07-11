use crate::{
    error::{Result, TelegramExportError},
    model::{
        Attachment, ImportSummary, ImportWarning, Message, ParsedExport, Poll, PollOption,
        ServiceEvent, SourceFile, TimelineItem, TimelineItemKind,
    },
};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

pub const SCHEMA_VERSION: i64 = 1;

pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS imports (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            input_path TEXT NOT NULL,
            output_path TEXT NOT NULL,
            mode TEXT NOT NULL,
            tool_version TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            status TEXT NOT NULL,
            files_seen INTEGER NOT NULL DEFAULT 0,
            files_imported INTEGER NOT NULL DEFAULT 0,
            files_skipped INTEGER NOT NULL DEFAULT 0,
            timeline_items INTEGER NOT NULL DEFAULT 0,
            messages INTEGER NOT NULL DEFAULT 0,
            service_events INTEGER NOT NULL DEFAULT 0,
            attachments INTEGER NOT NULL DEFAULT 0,
            warnings INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS source_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            import_id INTEGER NOT NULL,
            relative_path TEXT NOT NULL,
            checksum TEXT NOT NULL,
            file_size INTEGER NOT NULL,
            parse_order INTEGER NOT NULL,
            detected_chat_title TEXT,
            FOREIGN KEY(import_id) REFERENCES imports(id),
            UNIQUE(import_id, relative_path),
            UNIQUE(import_id, parse_order)
        );

        CREATE TABLE IF NOT EXISTS chats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            created_import_id INTEGER NOT NULL,
            FOREIGN KEY(created_import_id) REFERENCES imports(id)
        );

        CREATE TABLE IF NOT EXISTS chat_aliases (
            chat_id INTEGER NOT NULL,
            title TEXT NOT NULL,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            UNIQUE(chat_id, title)
        );

        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            display_name TEXT NOT NULL,
            UNIQUE(display_name)
        );

        CREATE TABLE IF NOT EXISTS timeline_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id INTEGER NOT NULL,
            source_file_id INTEGER NOT NULL,
            source_anchor TEXT,
            telegram_message_id INTEGER,
            ordinal INTEGER NOT NULL,
            item_kind TEXT NOT NULL,
            timestamp TEXT,
            original_timestamp TEXT,
            actor_user_id INTEGER,
            display_text TEXT,
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(source_file_id) REFERENCES source_files(id),
            FOREIGN KEY(actor_user_id) REFERENCES users(id),
            UNIQUE(chat_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timeline_item_id INTEGER NOT NULL UNIQUE,
            telegram_message_id INTEGER NOT NULL,
            sender_user_id INTEGER,
            sender_inferred INTEGER NOT NULL DEFAULT 0,
            edited_timestamp TEXT,
            plain_text TEXT,
            text_entities_json TEXT NOT NULL DEFAULT '[]',
            reply_to_message_id INTEGER,
            reply_to_peer_id TEXT,
            forwarded_from TEXT,
            forwarded_from_id TEXT,
            forwarded_date TEXT,
            saved_from TEXT,
            via_bot TEXT,
            author TEXT,
            inline_bot_buttons_json TEXT NOT NULL DEFAULT '[]',
            reactions_json TEXT NOT NULL DEFAULT '[]',
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(timeline_item_id) REFERENCES timeline_items(id),
            FOREIGN KEY(sender_user_id) REFERENCES users(id)
        );

        CREATE TABLE IF NOT EXISTS service_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timeline_item_id INTEGER NOT NULL UNIQUE,
            event_type TEXT NOT NULL,
            actor_user_id INTEGER,
            target_names_json TEXT NOT NULL DEFAULT '[]',
            display_text TEXT NOT NULL,
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(timeline_item_id) REFERENCES timeline_items(id),
            FOREIGN KEY(actor_user_id) REFERENCES users(id)
        );

        CREATE TABLE IF NOT EXISTS attachments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timeline_item_id INTEGER NOT NULL,
            attachment_kind TEXT NOT NULL,
            relative_path TEXT,
            thumbnail_path TEXT,
            mime_type TEXT,
            file_size INTEGER,
            duration_seconds INTEGER,
            title TEXT,
            width INTEGER,
            height INTEGER,
            spoiler INTEGER NOT NULL DEFAULT 0,
            ttl_seconds INTEGER,
            skip_reason TEXT,
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(timeline_item_id) REFERENCES timeline_items(id)
        );

        CREATE TABLE IF NOT EXISTS polls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timeline_item_id INTEGER NOT NULL UNIQUE,
            question TEXT NOT NULL,
            closed INTEGER,
            total_voters INTEGER,
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(timeline_item_id) REFERENCES timeline_items(id)
        );

        CREATE TABLE IF NOT EXISTS poll_options (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            poll_id INTEGER NOT NULL,
            option_index INTEGER NOT NULL,
            text TEXT NOT NULL,
            voters INTEGER,
            chosen INTEGER,
            extra_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(poll_id) REFERENCES polls(id),
            UNIQUE(poll_id, option_index)
        );

        CREATE TABLE IF NOT EXISTS group_memberships (
            chat_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            source_timeline_item_id INTEGER,
            FOREIGN KEY(chat_id) REFERENCES chats(id),
            FOREIGN KEY(user_id) REFERENCES users(id),
            FOREIGN KEY(source_timeline_item_id) REFERENCES timeline_items(id),
            UNIQUE(chat_id, user_id)
        );

        CREATE TABLE IF NOT EXISTS import_warnings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            import_id INTEGER NOT NULL,
            source_file_id INTEGER,
            timeline_item_id INTEGER,
            severity TEXT NOT NULL DEFAULT 'warning',
            warning_code TEXT NOT NULL,
            message TEXT NOT NULL,
            context_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(import_id) REFERENCES imports(id),
            FOREIGN KEY(source_file_id) REFERENCES source_files(id),
            FOREIGN KEY(timeline_item_id) REFERENCES timeline_items(id)
        );

        CREATE INDEX IF NOT EXISTS idx_source_files_checksum
            ON source_files(checksum);
        CREATE INDEX IF NOT EXISTS idx_timeline_items_chat_ordinal
            ON timeline_items(chat_id, ordinal);
        CREATE INDEX IF NOT EXISTS idx_messages_telegram_message_id
            ON messages(telegram_message_id);
        CREATE INDEX IF NOT EXISTS idx_attachments_timeline_item_id
            ON attachments(timeline_item_id);
        CREATE INDEX IF NOT EXISTS idx_import_warnings_import_id
            ON import_warnings(import_id);
        "#,
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub fn begin_import<I, O>(
    conn: &Connection,
    input_path: I,
    output_path: O,
    mode: &str,
) -> Result<i64>
where
    I: AsRef<Path>,
    O: AsRef<Path>,
{
    conn.execute(
        "INSERT INTO imports (
            input_path, output_path, mode, tool_version, started_at, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'running')",
        params![
            path_to_db(input_path.as_ref()),
            path_to_db(output_path.as_ref()),
            mode,
            env!("CARGO_PKG_VERSION"),
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_import(conn: &Connection, import_id: i64, summary: &ImportSummary) -> Result<()> {
    conn.execute(
        "UPDATE imports
         SET finished_at = ?1,
             status = 'finished',
             files_seen = ?2,
             files_imported = ?3,
             files_skipped = ?4,
             timeline_items = ?5,
             messages = ?6,
             service_events = ?7,
             attachments = ?8,
             warnings = ?9
         WHERE id = ?10",
        params![
            Utc::now().to_rfc3339(),
            usize_to_i64(summary.files_seen, "files_seen")?,
            usize_to_i64(summary.files_imported, "files_imported")?,
            usize_to_i64(summary.files_skipped, "files_skipped")?,
            usize_to_i64(summary.timeline_items, "timeline_items")?,
            usize_to_i64(summary.messages, "messages")?,
            usize_to_i64(summary.service_events, "service_events")?,
            usize_to_i64(summary.attachments, "attachments")?,
            usize_to_i64(summary.warnings, "warnings")?,
            import_id,
        ],
    )?;
    Ok(())
}

pub fn insert_source_file(
    conn: &Connection,
    import_id: i64,
    source_file: &SourceFile,
    chat_title: Option<&str>,
) -> Result<i64> {
    let relative_path = path_to_db(&source_file.relative_path);
    let parse_order = usize_to_i64(source_file.parse_order, "parse_order")?;
    let existing_id = conn
        .query_row(
            "SELECT id
             FROM source_files
             WHERE import_id = ?1
               AND (relative_path = ?2 OR parse_order = ?3)
             ORDER BY CASE WHEN relative_path = ?2 THEN 0 ELSE 1 END, id
             LIMIT 1",
            params![import_id, relative_path, parse_order],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE source_files
             SET relative_path = ?1,
                 checksum = ?2,
                 file_size = ?3,
                 parse_order = ?4,
                 detected_chat_title = ?5
             WHERE id = ?6",
            params![
                relative_path,
                source_file.checksum,
                u64_to_i64(source_file.file_size, "file_size")?,
                parse_order,
                chat_title,
                id,
            ],
        )?;
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO source_files (
            import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            import_id,
            relative_path,
            source_file.checksum,
            u64_to_i64(source_file.file_size, "file_size")?,
            parse_order,
            chat_title,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn find_source_file_by_checksum(conn: &Connection, checksum: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT sf.id
             FROM source_files sf
             JOIN imports i ON i.id = sf.import_id
             WHERE sf.checksum = ?1
               AND i.status = 'finished'
             ORDER BY sf.id
             LIMIT 1",
            [checksum],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn insert_parsed_export(
    conn: &Connection,
    import_id: i64,
    source_files: &[SourceFile],
    parsed: &ParsedExport,
) -> Result<()> {
    let chat_title = parsed
        .chat
        .as_ref()
        .map(|chat| chat.title.as_str())
        .unwrap_or("Unknown");
    let chat_id = ensure_chat(conn, import_id, chat_title)?;
    let source_file_ids =
        source_file_ids_for_parsed(conn, import_id, source_files, parsed, chat_title)?;
    let mut timeline_item_ids = HashMap::new();

    for item in &parsed.timeline_items {
        let timeline_item_id = insert_timeline_item(conn, chat_id, &source_file_ids, item)?;
        timeline_item_ids.insert(item.ordinal, timeline_item_id);
    }

    for message in &parsed.messages {
        insert_message(conn, &timeline_item_ids, message)?;
    }

    for service_event in &parsed.service_events {
        insert_service_event(conn, &timeline_item_ids, service_event)?;
    }

    for attachment in &parsed.attachments {
        insert_attachment(conn, &timeline_item_ids, attachment)?;
    }

    let mut poll_ids = HashMap::new();
    for poll in &parsed.polls {
        let poll_id = insert_poll(conn, &timeline_item_ids, poll)?;
        poll_ids.insert(poll.timeline_ordinal, poll_id);
    }

    for option in &parsed.poll_options {
        insert_poll_option(conn, &poll_ids, option)?;
    }

    for warning in &parsed.warnings {
        insert_warning(
            conn,
            import_id,
            &source_file_ids,
            &timeline_item_ids,
            warning,
        )?;
    }

    Ok(())
}

pub fn create_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS timeline_items_fts
            USING fts5(display_text, content='timeline_items', content_rowid='id');
        INSERT INTO timeline_items_fts(timeline_items_fts) VALUES('rebuild');
        "#,
    )?;
    Ok(())
}

fn source_file_ids_for_parsed(
    conn: &Connection,
    import_id: i64,
    source_files: &[SourceFile],
    parsed: &ParsedExport,
    chat_title: &str,
) -> Result<HashMap<usize, i64>> {
    let mut ids = HashMap::new();
    for parse_order in referenced_source_parse_orders(parsed) {
        let source_file = source_files
            .iter()
            .find(|source_file| source_file.parse_order == parse_order)
            .ok_or_else(|| {
                parse_error(format!(
                    "parsed export references source parse order {parse_order}, but no matching source file was supplied"
                ))
            })?;
        let id = insert_source_file(conn, import_id, source_file, Some(chat_title))?;
        ids.insert(parse_order, id);
    }
    Ok(ids)
}

fn referenced_source_parse_orders(parsed: &ParsedExport) -> BTreeSet<usize> {
    let mut parse_orders = BTreeSet::new();
    for item in &parsed.timeline_items {
        parse_orders.insert(item.source_file_parse_order);
    }
    for warning in &parsed.warnings {
        if let Some(parse_order) = warning.source_file_parse_order {
            parse_orders.insert(parse_order);
        }
    }
    parse_orders
}

fn insert_timeline_item(
    conn: &Connection,
    chat_id: i64,
    source_file_ids: &HashMap<usize, i64>,
    item: &TimelineItem,
) -> Result<i64> {
    let source_file_id = *source_file_ids
        .get(&item.source_file_parse_order)
        .ok_or_else(|| {
            parse_error(format!(
                "timeline item ordinal {} references unknown source parse order {}",
                item.ordinal, item.source_file_parse_order
            ))
        })?;
    let actor_user_id = optional_user_id(conn, item.actor_name.as_deref())?;
    let item_kind = timeline_item_kind(item.kind.clone());
    let extra_json = json_string(&item.extra_json)?;
    let existing_id = conn
        .query_row(
            "SELECT id FROM timeline_items WHERE chat_id = ?1 AND ordinal = ?2",
            params![chat_id, item.ordinal],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE timeline_items
             SET source_file_id = ?1,
                 source_anchor = ?2,
                 telegram_message_id = ?3,
                 item_kind = ?4,
                 timestamp = ?5,
                 original_timestamp = ?6,
                 actor_user_id = ?7,
                 display_text = ?8,
                 extra_json = ?9
             WHERE id = ?10",
            params![
                source_file_id,
                item.source_anchor,
                item.telegram_message_id,
                item_kind,
                item.timestamp,
                item.original_timestamp,
                actor_user_id,
                item.display_text,
                extra_json,
                id,
            ],
        )?;
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO timeline_items (
            chat_id, source_file_id, source_anchor, telegram_message_id, ordinal, item_kind,
            timestamp, original_timestamp, actor_user_id, display_text, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            chat_id,
            source_file_id,
            item.source_anchor,
            item.telegram_message_id,
            item.ordinal,
            item_kind,
            item.timestamp,
            item.original_timestamp,
            actor_user_id,
            item.display_text,
            extra_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_message(
    conn: &Connection,
    timeline_item_ids: &HashMap<i64, i64>,
    message: &Message,
) -> Result<()> {
    let timeline_item_id = timeline_id(timeline_item_ids, message.timeline_ordinal, "message")?;
    let sender_user_id = optional_user_id(conn, message.sender_name.as_deref())?;
    let text_entities_json = json_string(&message.text_entities)?;
    let inline_bot_buttons_json = json_string(&message.inline_bot_buttons)?;
    let reactions_json = json_string(&message.reactions)?;
    let extra_json = json_string(&message.extra_json)?;
    let existing_id = conn
        .query_row(
            "SELECT id FROM messages WHERE timeline_item_id = ?1",
            [timeline_item_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE messages
             SET telegram_message_id = ?1,
                 sender_user_id = ?2,
                 sender_inferred = ?3,
                 edited_timestamp = ?4,
                 plain_text = ?5,
                 text_entities_json = ?6,
                 reply_to_message_id = ?7,
                 reply_to_peer_id = ?8,
                 forwarded_from = ?9,
                 forwarded_from_id = ?10,
                 forwarded_date = ?11,
                 saved_from = ?12,
                 via_bot = ?13,
                 author = ?14,
                 inline_bot_buttons_json = ?15,
                 reactions_json = ?16,
                 extra_json = ?17
             WHERE id = ?18",
            params![
                message.telegram_message_id,
                sender_user_id,
                bool_to_i64(message.sender_inferred),
                message.edited_timestamp,
                message.plain_text,
                text_entities_json,
                message.reply_to_message_id,
                message.reply_to_peer_id,
                message.forwarded_from,
                message.forwarded_from_id,
                message.forwarded_date,
                message.saved_from,
                message.via_bot,
                message.author,
                inline_bot_buttons_json,
                reactions_json,
                extra_json,
                id,
            ],
        )?;
        return Ok(());
    }

    conn.execute(
        "INSERT INTO messages (
            timeline_item_id, telegram_message_id, sender_user_id, sender_inferred,
            edited_timestamp, plain_text, text_entities_json, reply_to_message_id,
            reply_to_peer_id, forwarded_from, forwarded_from_id, forwarded_date,
            saved_from, via_bot, author, inline_bot_buttons_json, reactions_json, extra_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
        params![
            timeline_item_id,
            message.telegram_message_id,
            sender_user_id,
            bool_to_i64(message.sender_inferred),
            message.edited_timestamp,
            message.plain_text,
            text_entities_json,
            message.reply_to_message_id,
            message.reply_to_peer_id,
            message.forwarded_from,
            message.forwarded_from_id,
            message.forwarded_date,
            message.saved_from,
            message.via_bot,
            message.author,
            inline_bot_buttons_json,
            reactions_json,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_service_event(
    conn: &Connection,
    timeline_item_ids: &HashMap<i64, i64>,
    service_event: &ServiceEvent,
) -> Result<()> {
    let timeline_item_id = timeline_id(
        timeline_item_ids,
        service_event.timeline_ordinal,
        "service event",
    )?;
    let actor_user_id = optional_user_id(conn, service_event.actor_name.as_deref())?;
    let target_names_json = json_string(&service_event.target_names)?;
    let extra_json = json_string(&service_event.extra_json)?;
    let existing_id = conn
        .query_row(
            "SELECT id FROM service_events WHERE timeline_item_id = ?1",
            [timeline_item_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE service_events
             SET event_type = ?1,
                 actor_user_id = ?2,
                 target_names_json = ?3,
                 display_text = ?4,
                 extra_json = ?5
             WHERE id = ?6",
            params![
                service_event.event_type,
                actor_user_id,
                target_names_json,
                service_event.display_text,
                extra_json,
                id,
            ],
        )?;
        return Ok(());
    }

    conn.execute(
        "INSERT INTO service_events (
            timeline_item_id, event_type, actor_user_id, target_names_json, display_text, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            timeline_item_id,
            service_event.event_type,
            actor_user_id,
            target_names_json,
            service_event.display_text,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_attachment(
    conn: &Connection,
    timeline_item_ids: &HashMap<i64, i64>,
    attachment: &Attachment,
) -> Result<()> {
    let timeline_item_id =
        timeline_id(timeline_item_ids, attachment.timeline_ordinal, "attachment")?;
    let relative_path = attachment.relative_path.as_deref().map(path_to_db);
    let thumbnail_path = attachment.thumbnail_path.as_deref().map(path_to_db);
    let file_size = attachment
        .file_size
        .map(|size| u64_to_i64(size, "attachment.file_size"))
        .transpose()?;
    let extra_json = json_string(&attachment.extra_json)?;

    conn.execute(
        "INSERT INTO attachments (
            timeline_item_id, attachment_kind, relative_path, thumbnail_path, mime_type,
            file_size, duration_seconds, title, width, height, spoiler, ttl_seconds,
            skip_reason, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            timeline_item_id,
            attachment.kind,
            relative_path,
            thumbnail_path,
            attachment.mime_type,
            file_size,
            attachment.duration_seconds,
            attachment.title,
            attachment.width,
            attachment.height,
            bool_to_i64(attachment.spoiler),
            attachment.ttl_seconds,
            attachment.skip_reason,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_poll(
    conn: &Connection,
    timeline_item_ids: &HashMap<i64, i64>,
    poll: &Poll,
) -> Result<i64> {
    let timeline_item_id = timeline_id(timeline_item_ids, poll.timeline_ordinal, "poll")?;
    let extra_json = json_string(&poll.extra_json)?;
    let existing_id = conn
        .query_row(
            "SELECT id FROM polls WHERE timeline_item_id = ?1",
            [timeline_item_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE polls
             SET question = ?1,
                 closed = ?2,
                 total_voters = ?3,
                 extra_json = ?4
             WHERE id = ?5",
            params![
                poll.question,
                poll.closed.map(bool_to_i64),
                poll.total_voters,
                extra_json,
                id,
            ],
        )?;
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO polls (
            timeline_item_id, question, closed, total_voters, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            timeline_item_id,
            poll.question,
            poll.closed.map(bool_to_i64),
            poll.total_voters,
            extra_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_poll_option(
    conn: &Connection,
    poll_ids: &HashMap<i64, i64>,
    option: &PollOption,
) -> Result<()> {
    let poll_id = *poll_ids.get(&option.timeline_ordinal).ok_or_else(|| {
        parse_error(format!(
            "poll option index {} references unknown poll timeline ordinal {}",
            option.option_index, option.timeline_ordinal
        ))
    })?;
    let extra_json = json_string(&option.extra_json)?;
    let existing_id = conn
        .query_row(
            "SELECT id FROM poll_options WHERE poll_id = ?1 AND option_index = ?2",
            params![poll_id, option.option_index],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE poll_options
             SET text = ?1,
                 voters = ?2,
                 chosen = ?3,
                 extra_json = ?4
             WHERE id = ?5",
            params![
                option.text,
                option.voters,
                option.chosen.map(bool_to_i64),
                extra_json,
                id,
            ],
        )?;
        return Ok(());
    }

    conn.execute(
        "INSERT INTO poll_options (
            poll_id, option_index, text, voters, chosen, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            poll_id,
            option.option_index,
            option.text,
            option.voters,
            option.chosen.map(bool_to_i64),
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_warning(
    conn: &Connection,
    import_id: i64,
    source_file_ids: &HashMap<usize, i64>,
    timeline_item_ids: &HashMap<i64, i64>,
    warning: &ImportWarning,
) -> Result<()> {
    let source_file_id = warning
        .source_file_parse_order
        .map(|parse_order| {
            source_file_ids.get(&parse_order).copied().ok_or_else(|| {
                parse_error(format!(
                    "warning references unknown source parse order {parse_order}"
                ))
            })
        })
        .transpose()?;
    let timeline_item_id = warning
        .timeline_ordinal
        .map(|ordinal| timeline_id(timeline_item_ids, ordinal, "warning"))
        .transpose()?;
    let context_json = json_string(&warning.context)?;

    conn.execute(
        "INSERT INTO import_warnings (
            import_id, source_file_id, timeline_item_id, severity, warning_code, message,
            context_json
         ) VALUES (?1, ?2, ?3, 'warning', ?4, ?5, ?6)",
        params![
            import_id,
            source_file_id,
            timeline_item_id,
            warning.code.as_str(),
            warning.message,
            context_json,
        ],
    )?;
    Ok(())
}

pub struct AttachmentMediaRow {
    pub id: i64,
    pub timeline_item_id: i64,
    pub relative_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub extra_json: String,
}

pub fn load_attachment_media(conn: &Connection) -> Result<Vec<AttachmentMediaRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, timeline_item_id, relative_path, thumbnail_path, extra_json
         FROM attachments ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(AttachmentMediaRow {
            id: row.get(0)?,
            timeline_item_id: row.get(1)?,
            relative_path: row.get(2)?,
            thumbnail_path: row.get(3)?,
            extra_json: row.get(4)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn update_attachment_media(
    conn: &Connection,
    id: i64,
    relative_path: Option<&str>,
    thumbnail_path: Option<&str>,
    extra_json: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE attachments
         SET relative_path = ?2, thumbnail_path = ?3, extra_json = ?4
         WHERE id = ?1",
        params![id, relative_path, thumbnail_path, extra_json],
    )?;
    Ok(())
}

/// Add `delta` to `imports.warnings` for `import_id`. Used by bundle
/// relocation, which runs after `finish_import` has already written the
/// warnings count, to fold in the warnings it discovers along the way.
pub fn add_import_warnings(conn: &Connection, import_id: i64, delta: usize) -> Result<()> {
    conn.execute(
        "UPDATE imports SET warnings = warnings + ?2 WHERE id = ?1",
        params![import_id, usize_to_i64(delta, "warnings")?],
    )?;
    Ok(())
}

pub fn latest_import_id(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT id FROM imports ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?)
}

pub fn insert_media_warning(
    conn: &Connection,
    import_id: i64,
    timeline_item_id: i64,
    warning_code: &str,
    message: &str,
    context_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO import_warnings
            (import_id, source_file_id, timeline_item_id, severity, warning_code, message, context_json)
         VALUES (?1, NULL, ?2, 'warning', ?3, ?4, ?5)",
        params![import_id, timeline_item_id, warning_code, message, context_json],
    )?;
    Ok(())
}

fn ensure_chat(conn: &Connection, import_id: i64, title: &str) -> Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT chat_id FROM chat_aliases WHERE title = ?1 ORDER BY chat_id LIMIT 1",
            [title],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }

    if let Some(id) = conn
        .query_row(
            "SELECT id FROM chats WHERE title = ?1 ORDER BY id LIMIT 1",
            [title],
            |row| row.get(0),
        )
        .optional()?
    {
        conn.execute(
            "INSERT OR IGNORE INTO chat_aliases (chat_id, title) VALUES (?1, ?2)",
            params![id, title],
        )?;
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO chats (title, created_import_id) VALUES (?1, ?2)",
        params![title, import_id],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT OR IGNORE INTO chat_aliases (chat_id, title) VALUES (?1, ?2)",
        params![id, title],
    )?;
    Ok(id)
}

fn optional_user_id(conn: &Connection, display_name: Option<&str>) -> Result<Option<i64>> {
    display_name
        .filter(|name| !name.is_empty())
        .map(|name| ensure_user(conn, name))
        .transpose()
}

fn ensure_user(conn: &Connection, display_name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO users (display_name) VALUES (?1)",
        [display_name],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM users WHERE display_name = ?1",
        [display_name],
        |row| row.get(0),
    )?)
}

fn timeline_id(
    timeline_item_ids: &HashMap<i64, i64>,
    ordinal: i64,
    child_kind: &str,
) -> Result<i64> {
    timeline_item_ids.get(&ordinal).copied().ok_or_else(|| {
        parse_error(format!(
            "{child_kind} references unknown timeline ordinal {ordinal}"
        ))
    })
}

fn timeline_item_kind(kind: TimelineItemKind) -> &'static str {
    match kind {
        TimelineItemKind::Message => "message",
        TimelineItemKind::ServiceEvent => "service_event",
        TimelineItemKind::Unsupported => "unsupported",
        TimelineItemKind::DateSeparator => "date_separator",
    }
}

fn json_string<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn path_to_db(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn usize_to_i64(value: usize, field_name: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        parse_error(format!(
            "{field_name} value {value} does not fit in SQLite INTEGER"
        ))
    })
}

fn u64_to_i64(value: u64, field_name: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        parse_error(format!(
            "{field_name} value {value} does not fit in SQLite INTEGER"
        ))
    })
}

fn parse_error(message: String) -> TelegramExportError {
    TelegramExportError::Parse(message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::model::{
        Attachment, Chat, ImportSummary, ImportWarning, Message, ParsedExport, Poll, PollOption,
        ServiceEvent, SourceFile, TextEntity, TextEntityKind, TimelineItem, TimelineItemKind,
        WarningCode,
    };

    fn table_names(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
    }

    fn source_file_indexes(conn: &Connection) -> Vec<(String, bool)> {
        conn.prepare("PRAGMA index_list('source_files')")
            .unwrap()
            .query_map([], |row| {
                let name: String = row.get(1)?;
                let unique: i64 = row.get(2)?;
                Ok((name, unique != 0))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn index_columns(conn: &Connection, index_name: &str) -> Vec<String> {
        let pragma = format!("PRAGMA index_info('{index_name}')");
        conn.prepare(&pragma)
            .unwrap()
            .query_map([], |row| row.get(2))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn source_file(relative_path: &str, checksum: &str, parse_order: usize) -> SourceFile {
        SourceFile {
            absolute_path: PathBuf::from(format!("/tmp/export/{relative_path}")),
            relative_path: PathBuf::from(relative_path),
            checksum: checksum.to_string(),
            file_size: 1234,
            parse_order,
        }
    }

    fn timeline_item(source_file_parse_order: usize, ordinal: i64) -> TimelineItem {
        TimelineItem {
            id: None,
            source_file_parse_order,
            source_anchor: Some(format!("message-{ordinal}")),
            telegram_message_id: Some(ordinal),
            ordinal,
            kind: TimelineItemKind::Message,
            timestamp: Some("2026-06-01T12:00:00Z".to_string()),
            original_timestamp: None,
            actor_name: None,
            display_text: Some(format!("Message {ordinal}")),
            extra_json: json!({}),
        }
    }

    #[test]
    fn creates_expected_tables_indexes_and_pragmas() {
        let conn = Connection::open_in_memory().unwrap();

        create_schema(&conn).unwrap();

        let tables = table_names(&conn);
        for table in [
            "imports",
            "source_files",
            "chats",
            "chat_aliases",
            "users",
            "timeline_items",
            "messages",
            "service_events",
            "attachments",
            "polls",
            "poll_options",
            "group_memberships",
            "import_warnings",
        ] {
            assert!(
                tables.contains(&table.to_string()),
                "missing table {table}; found {tables:?}"
            );
        }

        let indexes = index_names(&conn);
        for index in [
            "idx_source_files_checksum",
            "idx_timeline_items_chat_ordinal",
            "idx_messages_telegram_message_id",
            "idx_attachments_timeline_item_id",
            "idx_import_warnings_import_id",
        ] {
            assert!(
                indexes.contains(&index.to_string()),
                "missing index {index}; found {indexes:?}"
            );
        }

        let schema_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(schema_version, SCHEMA_VERSION);

        let foreign_keys_enabled: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys_enabled, 1);
    }

    #[test]
    fn source_files_checksum_index_is_non_unique() {
        let conn = Connection::open_in_memory().unwrap();

        create_schema(&conn).unwrap();

        let indexes = source_file_indexes(&conn);
        assert!(
            indexes
                .iter()
                .any(|(name, unique)| name == "idx_source_files_checksum" && !unique),
            "expected non-unique idx_source_files_checksum; found {indexes:?}"
        );

        let unique_checksum_indexes: Vec<String> = indexes
            .iter()
            .filter(|(_, unique)| *unique)
            .filter_map(|(name, _)| {
                if index_columns(&conn, name) == ["checksum"] {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            unique_checksum_indexes.is_empty(),
            "checksum must not have a global unique index; found {unique_checksum_indexes:?}"
        );
    }

    #[test]
    fn does_not_stamp_schema_version_when_schema_creation_fails() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE idx_timeline_items_chat_ordinal (id INTEGER)",
            [],
        )
        .unwrap();

        let result = create_schema(&conn);

        assert!(result.is_err());
        let schema_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(schema_version, 0);
    }

    #[test]
    fn inserts_parsed_export_and_optional_fts() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let source = source_file("chat_001/messages.html", "checksum-one", 0);
        let source_id =
            insert_source_file(&conn, import_id, &source, Some("Example Chat")).unwrap();
        let parsed = ParsedExport {
            chat: Some(Chat {
                title: "Example Chat".to_string(),
            }),
            timeline_items: vec![
                TimelineItem {
                    id: None,
                    source_file_parse_order: 0,
                    source_anchor: Some("message-101".to_string()),
                    telegram_message_id: Some(101),
                    ordinal: 1,
                    kind: TimelineItemKind::Message,
                    timestamp: Some("2026-06-01T12:00:00Z".to_string()),
                    original_timestamp: Some("1 June 2026, 12:00".to_string()),
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Hello from Telegram export".to_string()),
                    extra_json: json!({ "css": ["message", "default"] }),
                },
                TimelineItem {
                    id: None,
                    source_file_parse_order: 0,
                    source_anchor: Some("message-102".to_string()),
                    telegram_message_id: Some(102),
                    ordinal: 2,
                    kind: TimelineItemKind::ServiceEvent,
                    timestamp: Some("2026-06-01T12:05:00Z".to_string()),
                    original_timestamp: None,
                    actor_name: Some("Carol".to_string()),
                    display_text: Some("Carol pinned a poll".to_string()),
                    extra_json: json!({ "service": true }),
                },
            ],
            messages: vec![Message {
                timeline_ordinal: 1,
                telegram_message_id: 101,
                sender_name: Some("Bob".to_string()),
                sender_inferred: true,
                edited_timestamp: Some("2026-06-01T12:02:00Z".to_string()),
                plain_text: Some("Hello from Telegram export".to_string()),
                text_entities: vec![TextEntity {
                    kind: TextEntityKind::TextUrl,
                    text: "Telegram".to_string(),
                    extra: json!({ "href": "https://telegram.org" }),
                }],
                reply_to_message_id: Some(100),
                reply_to_peer_id: Some("peer-1".to_string()),
                forwarded_from: Some("Forwarded User".to_string()),
                forwarded_from_id: Some("user123".to_string()),
                forwarded_date: Some("2026-05-31T09:00:00Z".to_string()),
                saved_from: Some("Saved Chat".to_string()),
                via_bot: Some("ExampleBot".to_string()),
                author: Some("Channel Author".to_string()),
                inline_bot_buttons: json!([{ "text": "Open", "url": "https://example.com" }]),
                reactions: json!([{ "emoji": "thumbs_up", "count": 3 }]),
                extra_json: json!({ "message_extra": "kept" }),
            }],
            service_events: vec![ServiceEvent {
                timeline_ordinal: 2,
                event_type: "pin_message".to_string(),
                actor_name: Some("Carol".to_string()),
                target_names: vec!["Bob".to_string(), "Alice".to_string()],
                display_text: "Carol pinned a poll".to_string(),
                extra_json: json!({ "raw": "service text" }),
            }],
            attachments: vec![Attachment {
                timeline_ordinal: 1,
                kind: "photo".to_string(),
                relative_path: Some(PathBuf::from("chat_001/photos/photo_1.jpg")),
                thumbnail_path: Some(PathBuf::from("chat_001/photos/thumb_1.jpg")),
                mime_type: Some("image/jpeg".to_string()),
                file_size: Some(4567),
                duration_seconds: None,
                title: Some("photo_1.jpg".to_string()),
                width: Some(640),
                height: Some(480),
                spoiler: true,
                ttl_seconds: Some(30),
                skip_reason: None,
                extra_json: json!({ "media_id": "abc" }),
            }],
            polls: vec![Poll {
                timeline_ordinal: 1,
                question: "Pick one".to_string(),
                closed: Some(false),
                total_voters: Some(7),
                extra_json: json!({ "poll_id": "poll-1" }),
            }],
            poll_options: vec![
                PollOption {
                    timeline_ordinal: 1,
                    option_index: 0,
                    text: "Yes".to_string(),
                    voters: Some(5),
                    chosen: Some(true),
                    extra_json: json!({ "winner": true }),
                },
                PollOption {
                    timeline_ordinal: 1,
                    option_index: 1,
                    text: "No".to_string(),
                    voters: Some(2),
                    chosen: Some(false),
                    extra_json: json!({ "winner": false }),
                },
            ],
            warnings: vec![ImportWarning {
                source_file_parse_order: Some(0),
                timeline_ordinal: Some(1),
                code: WarningCode::MissingAttachment,
                message: "attachment was referenced but missing".to_string(),
                context: json!({ "path": "missing.bin" }),
            }],
        };

        insert_parsed_export(&conn, import_id, std::slice::from_ref(&source), &parsed).unwrap();
        finish_import(
            &conn,
            import_id,
            &ImportSummary {
                files_seen: 1,
                files_imported: 1,
                files_skipped: 0,
                timeline_items: 2,
                messages: 1,
                service_events: 1,
                attachments: 1,
                warnings: 1,
            },
        )
        .unwrap();
        create_fts(&conn).unwrap();
        create_fts(&conn).unwrap();

        let import_row: (String, i64, i64, i64) = conn
            .query_row(
                "SELECT status, timeline_items, messages, warnings FROM imports WHERE id = ?1",
                [import_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(import_row, ("finished".to_string(), 2, 1, 1));

        let timeline_row: (i64, i64, String, String, String) = conn
            .query_row(
                "SELECT chat_id, source_file_id, item_kind, display_text, extra_json
                 FROM timeline_items WHERE ordinal = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(timeline_row.1, source_id);
        assert_eq!(timeline_row.2, "message");
        assert_eq!(timeline_row.3, "Hello from Telegram export");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&timeline_row.4).unwrap(),
            json!({ "css": ["message", "default"] })
        );

        let user_names: Vec<String> = conn
            .prepare("SELECT display_name FROM users ORDER BY display_name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(user_names, ["Alice", "Bob", "Carol"]);

        let message_json: (i64, String, String, String) = conn
            .query_row(
                "SELECT sender_inferred, text_entities_json, inline_bot_buttons_json, reactions_json
                 FROM messages WHERE telegram_message_id = 101",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(message_json.0, 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&message_json.1).unwrap(),
            json!([{ "type": "text_link", "text": "Telegram", "href": "https://telegram.org" }])
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&message_json.2).unwrap(),
            json!([{ "text": "Open", "url": "https://example.com" }])
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&message_json.3).unwrap(),
            json!([{ "emoji": "thumbs_up", "count": 3 }])
        );

        let attachment_path: String = conn
            .query_row("SELECT relative_path FROM attachments", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(attachment_path, "chat_001/photos/photo_1.jpg");

        let poll_rows: Vec<(i64, String, i64)> = conn
            .prepare(
                "SELECT po.option_index, po.text, po.chosen
                 FROM poll_options po
                 JOIN polls p ON p.id = po.poll_id
                 WHERE p.question = 'Pick one'
                 ORDER BY po.option_index",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            poll_rows,
            vec![(0, "Yes".to_string(), 1), (1, "No".to_string(), 0)]
        );

        let warning_link: (i64, i64, String, String) = conn
            .query_row(
                "SELECT source_file_id, timeline_item_id, warning_code, context_json
                 FROM import_warnings WHERE import_id = ?1",
                [import_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(warning_link.0, source_id);
        assert_eq!(warning_link.2, WarningCode::MissingAttachment.as_str());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&warning_link.3).unwrap(),
            json!({ "path": "missing.bin" })
        );

        let fts_matches: Vec<(i64, String)> = conn
            .prepare(
                "SELECT rowid, display_text FROM timeline_items_fts
                 WHERE timeline_items_fts MATCH 'Telegram'
                 ORDER BY rowid",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            fts_matches,
            vec![(warning_link.1, "Hello from Telegram export".to_string())]
        );
    }

    #[test]
    fn inserts_duplicate_checksums_in_same_import_as_distinct_source_files() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let first = source_file("chat_001/messages.html", "same-checksum", 0);
        let second = source_file("chat_001/messages2.html", "same-checksum", 1);

        let first_id = insert_source_file(&conn, import_id, &first, Some("Example Chat")).unwrap();
        let second_id =
            insert_source_file(&conn, import_id, &second, Some("Example Chat")).unwrap();

        assert_ne!(first_id, second_id);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source_files WHERE import_id = ?1 AND checksum = ?2",
                (import_id, "same-checksum"),
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn finds_a_source_file_by_checksum_when_duplicates_exist() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let first = source_file("chat_001/messages.html", "same-checksum", 0);
        let second = source_file("chat_001/messages2.html", "same-checksum", 1);
        let first_id = insert_source_file(&conn, import_id, &first, Some("Example Chat")).unwrap();
        let second_id =
            insert_source_file(&conn, import_id, &second, Some("Example Chat")).unwrap();
        finish_import(&conn, import_id, &ImportSummary::default()).unwrap();

        let found = find_source_file_by_checksum(&conn, "same-checksum").unwrap();

        assert!(matches!(found, Some(id) if id == first_id || id == second_id));
    }

    #[test]
    fn ignores_source_file_checksum_from_running_import() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let source = source_file("chat_001/messages.html", "running-checksum", 0);
        insert_source_file(&conn, import_id, &source, Some("Example Chat")).unwrap();

        let found = find_source_file_by_checksum(&conn, "running-checksum").unwrap();

        assert_eq!(found, None);
    }

    #[test]
    fn finds_source_file_checksum_after_import_finishes() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let source = source_file("chat_001/messages.html", "finished-checksum", 0);
        let source_id =
            insert_source_file(&conn, import_id, &source, Some("Example Chat")).unwrap();
        assert_eq!(
            find_source_file_by_checksum(&conn, "finished-checksum").unwrap(),
            None
        );

        finish_import(&conn, import_id, &ImportSummary::default()).unwrap();

        assert_eq!(
            find_source_file_by_checksum(&conn, "finished-checksum").unwrap(),
            Some(source_id)
        );
    }

    #[test]
    fn ignores_duplicate_source_file_checksums_from_running_import() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let first = source_file("chat_001/messages.html", "running-duplicate-checksum", 0);
        let second = source_file("chat_001/messages2.html", "running-duplicate-checksum", 1);
        insert_source_file(&conn, import_id, &first, Some("Example Chat")).unwrap();
        insert_source_file(&conn, import_id, &second, Some("Example Chat")).unwrap();

        let found = find_source_file_by_checksum(&conn, "running-duplicate-checksum").unwrap();

        assert_eq!(found, None);
    }

    #[test]
    fn insert_parsed_export_only_records_referenced_source_files() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let first = source_file("chat_001/messages.html", "checksum-one", 0);
        let second = source_file("chat_001/messages2.html", "checksum-two", 1);
        let parsed = ParsedExport {
            chat: Some(Chat {
                title: "Parsed Chat".to_string(),
            }),
            timeline_items: vec![timeline_item(0, 1)],
            ..ParsedExport::default()
        };

        insert_parsed_export(&conn, import_id, &[first, second], &parsed).unwrap();

        let rows: Vec<(i64, String)> = conn
            .prepare(
                "SELECT parse_order, detected_chat_title
                 FROM source_files
                 WHERE import_id = ?1
                 ORDER BY parse_order",
            )
            .unwrap()
            .query_map([import_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![(0, "Parsed Chat".to_string())]);
    }

    #[test]
    fn insert_parsed_export_errors_when_referenced_source_file_is_missing() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        let parsed = ParsedExport {
            chat: Some(Chat {
                title: "Parsed Chat".to_string(),
            }),
            timeline_items: vec![timeline_item(1, 1)],
            ..ParsedExport::default()
        };

        let err = insert_parsed_export(
            &conn,
            import_id,
            &[source_file("chat_001/messages.html", "checksum-one", 0)],
            &parsed,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            TelegramExportError::Parse(message)
                if message == "parsed export references source parse order 1, but no matching source file was supplied"
        ));
    }

    #[test]
    fn attachment_media_helpers_round_trip() {
        let staged = crate::importer::tests::staged_export(&crate::importer::tests::fixture_dir());
        crate::importer::run_import(crate::importer::tests::import_options(
            staged.path(),
            true,
            false,
            false,
        ))
        .unwrap();
        let conn = Connection::open(staged.path().join("chat.sqlite")).unwrap();

        let rows = load_attachment_media(&conn).unwrap();
        assert!(!rows.is_empty());
        assert!(latest_import_id(&conn).unwrap() >= 1);

        let first = rows[0].id;
        let timeline_item_id = rows[0].timeline_item_id;
        update_attachment_media(&conn, first, Some("assets/x.jpg"), None, "{\"bundle\":{}}")
            .unwrap();
        insert_media_warning(
            &conn,
            latest_import_id(&conn).unwrap(),
            timeline_item_id,
            "missing_attachment",
            "gone",
            "{}",
        )
        .unwrap();

        let rows = load_attachment_media(&conn).unwrap();
        assert!(
            rows.iter()
                .any(|r| r.relative_path.as_deref() == Some("assets/x.jpg"))
        );
        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'missing_attachment'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 1);
    }

    #[test]
    fn add_import_warnings_increments_warnings_column() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let import_id =
            begin_import(&conn, "fixtures/basic_export", "output.sqlite", "full").unwrap();
        finish_import(
            &conn,
            import_id,
            &ImportSummary {
                warnings: 2,
                ..ImportSummary::default()
            },
        )
        .unwrap();

        add_import_warnings(&conn, import_id, 3).unwrap();

        let warnings: i64 = conn
            .query_row(
                "SELECT warnings FROM imports WHERE id = ?1",
                [import_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(warnings, 5);
    }
}
