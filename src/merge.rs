use crate::{
    db,
    error::{Result, TelegramExportError},
    model::{ImportSummary, MergeOptions, MergeSummary},
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    result,
    time::{SystemTime, UNIX_EPOCH},
};

const REQUIRED_TABLES: &[&str] = &[
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
];

#[derive(Debug, Clone)]
struct SourceFileRow {
    id: i64,
    import_id: i64,
    relative_path: String,
    checksum: String,
    file_size: i64,
    parse_order: i64,
    detected_chat_title: Option<String>,
}

#[derive(Debug, Clone)]
struct TimelineRow {
    id: i64,
    chat_title: String,
    source_file_id: i64,
    source_anchor: Option<String>,
    telegram_message_id: Option<i64>,
    item_kind: String,
    timestamp: Option<String>,
    original_timestamp: Option<String>,
    actor_name: Option<String>,
    display_text: Option<String>,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct MessageRow {
    timeline_item_id: i64,
    telegram_message_id: i64,
    sender_name: Option<String>,
    sender_inferred: i64,
    edited_timestamp: Option<String>,
    plain_text: Option<String>,
    text_entities_json: String,
    reply_to_message_id: Option<i64>,
    reply_to_peer_id: Option<String>,
    forwarded_from: Option<String>,
    forwarded_from_id: Option<String>,
    forwarded_date: Option<String>,
    saved_from: Option<String>,
    via_bot: Option<String>,
    author: Option<String>,
    inline_bot_buttons_json: String,
    reactions_json: String,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct ServiceEventRow {
    timeline_item_id: i64,
    event_type: String,
    actor_name: Option<String>,
    target_names_json: String,
    display_text: String,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct AttachmentRow {
    timeline_item_id: i64,
    attachment_kind: String,
    relative_path: Option<String>,
    thumbnail_path: Option<String>,
    mime_type: Option<String>,
    file_size: Option<i64>,
    duration_seconds: Option<i64>,
    title: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    spoiler: i64,
    ttl_seconds: Option<i64>,
    skip_reason: Option<String>,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct PollRow {
    id: i64,
    timeline_item_id: i64,
    question: String,
    closed: Option<i64>,
    total_voters: Option<i64>,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct PollOptionRow {
    poll_id: i64,
    poll_timeline_item_id: i64,
    option_index: i64,
    text: String,
    voters: Option<i64>,
    chosen: Option<i64>,
    extra_json: String,
}

#[derive(Debug, Clone)]
struct ImportWarningRow {
    source_file_id: Option<i64>,
    timeline_item_id: Option<i64>,
    severity: String,
    warning_code: String,
    message: String,
    context_json: String,
}

#[derive(Debug, Clone)]
struct MergeSource {
    input_index: usize,
    input_path: String,
    source_import_id: i64,
    source_source_file_id: i64,
    source_timeline_item_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TimelineFingerprint(String);

pub fn run_merge(options: MergeOptions) -> Result<MergeSummary> {
    validate_merge_options(&options)?;
    let create_fts = options.fts || inputs_have_fts(&options.input_dbs)?;
    run_merge_safely(&options, create_fts)
}

fn validate_merge_options(options: &MergeOptions) -> Result<()> {
    if options.input_dbs.is_empty() {
        return Err(TelegramExportError::MergeRequiresInput);
    }

    if options.output_db.exists() && !options.force {
        return Err(TelegramExportError::OutputDatabaseExists(
            options.output_db.clone(),
        ));
    }

    let output_identity = path_identity(&options.output_db)?;
    for input in &options.input_dbs {
        let input_identity = path_identity(input)?;
        if output_identity == input_identity {
            return Err(TelegramExportError::MergeOutputIsInput {
                output: options.output_db.clone(),
                input: input.clone(),
            });
        }
        validate_input_database(input)?;
    }

    Ok(())
}

fn validate_input_database(path: &Path) -> Result<()> {
    let conn = open_input_database(path)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != db::SCHEMA_VERSION {
        return Err(TelegramExportError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            version,
        });
    }

    for table in REQUIRED_TABLES {
        let exists: i64 = conn.query_row(
            "SELECT EXISTS (
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table'
                  AND name = ?1
            )",
            [*table],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(TelegramExportError::MissingRequiredTable {
                path: path.to_path_buf(),
                table,
            });
        }
    }

    Ok(())
}

fn inputs_have_fts(input_dbs: &[PathBuf]) -> Result<bool> {
    for input in input_dbs {
        let conn = open_input_database(input)?;
        let exists: i64 = conn.query_row(
            "SELECT EXISTS (
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table'
                  AND name = 'timeline_items_fts'
            )",
            [],
            |row| row.get(0),
        )?;
        if exists != 0 {
            return Ok(true);
        }
    }

    Ok(false)
}

fn run_merge_safely(options: &MergeOptions, create_fts: bool) -> Result<MergeSummary> {
    let temp_path = temporary_database_path(&options.output_db);
    let merge_result = merge_to_database(options, &temp_path, create_fts).and_then(|summary| {
        replace_database(&temp_path, &options.output_db)?;
        cleanup_temp_database(&temp_path);
        Ok(summary)
    });

    if merge_result.is_err() {
        cleanup_temp_database(&temp_path);
    }

    merge_result
}

fn merge_to_database(
    options: &MergeOptions,
    database_path: &Path,
    create_fts: bool,
) -> Result<MergeSummary> {
    let mut conn = Connection::open(database_path)?;
    db::create_schema(&conn)?;
    let tx = conn.transaction()?;
    let mut merge_state = MergeState::default();

    for (input_index, input_path) in options.input_dbs.iter().enumerate() {
        let input_conn = open_input_database(input_path)?;
        merge_one_input(
            &tx,
            options,
            &input_conn,
            input_index,
            input_path,
            &mut merge_state,
        )?;
    }

    if create_fts {
        db::create_fts(&tx)?;
    }
    tx.commit()?;

    Ok(MergeSummary {
        input_databases: options.input_dbs.len(),
        timeline_items: merge_state.timeline_items,
        messages: merge_state.messages,
        service_events: merge_state.service_events,
        attachments: merge_state.attachments,
        duplicates_skipped: merge_state.duplicates_skipped,
        conflicts_kept: merge_state.conflicts_kept,
        warnings: merge_state.warnings,
    })
}

#[derive(Debug, Default)]
struct MergeState {
    next_ordinal: i64,
    seen_fingerprints: HashSet<TimelineFingerprint>,
    seen_message_ids: HashMap<i64, TimelineFingerprint>,
    timeline_items: usize,
    messages: usize,
    service_events: usize,
    attachments: usize,
    duplicates_skipped: usize,
    conflicts_kept: usize,
    warnings: usize,
}

fn merge_one_input(
    output_conn: &Connection,
    options: &MergeOptions,
    input_conn: &Connection,
    input_index: usize,
    input_path: &Path,
    merge_state: &mut MergeState,
) -> Result<()> {
    let input_path_text = path_to_db(input_path);
    let import_id = db::begin_import(output_conn, input_path, &options.output_db, "merge_input")?;
    let source_files = read_source_files(input_conn)?;
    let source_file_ids = copy_source_files(output_conn, import_id, &source_files)?;
    let timeline_rows = read_timeline_rows(input_conn)?;
    let message_rows = read_message_rows(input_conn)?;
    let service_event_rows = read_service_event_rows(input_conn)?;
    let attachment_rows = read_attachment_rows(input_conn)?;
    let poll_rows = read_poll_rows(input_conn)?;
    let poll_option_rows = read_poll_option_rows(input_conn)?;
    let warning_rows = read_import_warning_rows(input_conn)?;
    let message_rows_by_timeline: HashMap<i64, &MessageRow> = message_rows
        .iter()
        .map(|message| (message.timeline_item_id, message))
        .collect();
    let service_event_rows_by_timeline: HashMap<i64, &ServiceEventRow> = service_event_rows
        .iter()
        .map(|event| (event.timeline_item_id, event))
        .collect();
    let attachment_rows_by_timeline = attachment_refs_by_timeline(&attachment_rows);
    let mut timeline_item_ids = HashMap::new();
    let mut input_summary = ImportSummary {
        files_seen: source_files.len(),
        files_imported: source_files.len(),
        ..Default::default()
    };

    for row in timeline_rows {
        let source_file = source_files
            .iter()
            .find(|source_file| source_file.id == row.source_file_id)
            .ok_or_else(|| {
                parse_error(format!(
                    "input timeline row {} references missing source file {}",
                    row.id, row.source_file_id
                ))
            })?;
        let source_file_id = *source_file_ids.get(&row.source_file_id).ok_or_else(|| {
            parse_error(format!(
                "input source file {} was not copied",
                row.source_file_id
            ))
        })?;
        let source = MergeSource {
            input_index: input_index + 1,
            input_path: input_path_text.clone(),
            source_import_id: source_file.import_id,
            source_source_file_id: row.source_file_id,
            source_timeline_item_id: row.id,
        };
        let attachments = attachment_rows_by_timeline
            .get(&row.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let fingerprint = timeline_fingerprint(
            &row,
            message_rows_by_timeline.get(&row.id).copied(),
            service_event_rows_by_timeline.get(&row.id).copied(),
            attachments,
        )?;
        if !merge_state.seen_fingerprints.insert(fingerprint.clone()) {
            insert_merge_warning(
                output_conn,
                import_id,
                None,
                "merge_duplicate_skipped",
                "skipped duplicate timeline row during merge",
                &fingerprint,
                &source,
            )?;
            merge_state.duplicates_skipped += 1;
            merge_state.warnings += 1;
            input_summary.warnings += 1;
            continue;
        }
        let message_id_conflict =
            message_id_conflicts(&mut merge_state.seen_message_ids, &row, &fingerprint);
        let chat_id = ensure_output_chat(output_conn, import_id, &row.chat_title)?;
        let actor_user_id = ensure_output_user(output_conn, row.actor_name.as_deref())?;
        let timeline_item_id = insert_timeline_row(
            output_conn,
            chat_id,
            source_file_id,
            merge_state.next_ordinal,
            &row,
            actor_user_id,
            &source,
        )?;
        timeline_item_ids.insert(row.id, timeline_item_id);
        merge_state.next_ordinal += 1;
        merge_state.timeline_items += 1;
        input_summary.timeline_items += 1;
        if message_id_conflict {
            insert_merge_warning(
                output_conn,
                import_id,
                Some(timeline_item_id),
                "merge_message_id_conflict",
                "kept conflicting timeline row with reused Telegram message id",
                &fingerprint,
                &source,
            )?;
            merge_state.conflicts_kept += 1;
            merge_state.warnings += 1;
            input_summary.warnings += 1;
        }
    }

    for message in message_rows {
        let Some(timeline_item_id) = timeline_item_ids.get(&message.timeline_item_id).copied()
        else {
            continue;
        };
        let source = merge_source_for_child(
            input_index + 1,
            &input_path_text,
            input_conn,
            message.timeline_item_id,
        )?;
        insert_message_row(output_conn, timeline_item_id, &message, &source)?;
        merge_state.messages += 1;
        input_summary.messages += 1;
    }

    for service_event in service_event_rows {
        let Some(timeline_item_id) = timeline_item_ids
            .get(&service_event.timeline_item_id)
            .copied()
        else {
            continue;
        };
        let source = merge_source_for_child(
            input_index + 1,
            &input_path_text,
            input_conn,
            service_event.timeline_item_id,
        )?;
        insert_service_event_row(output_conn, timeline_item_id, &service_event, &source)?;
        merge_state.service_events += 1;
        input_summary.service_events += 1;
    }

    for attachment in &attachment_rows {
        let Some(timeline_item_id) = timeline_item_ids.get(&attachment.timeline_item_id).copied()
        else {
            continue;
        };
        let source = merge_source_for_child(
            input_index + 1,
            &input_path_text,
            input_conn,
            attachment.timeline_item_id,
        )?;
        insert_attachment_row(output_conn, timeline_item_id, attachment, &source)?;
        merge_state.attachments += 1;
        input_summary.attachments += 1;
    }

    let mut poll_ids = HashMap::new();
    for poll in poll_rows {
        let Some(timeline_item_id) = timeline_item_ids.get(&poll.timeline_item_id).copied() else {
            continue;
        };
        let source = merge_source_for_child(
            input_index + 1,
            &input_path_text,
            input_conn,
            poll.timeline_item_id,
        )?;
        let poll_id = insert_poll_row(output_conn, timeline_item_id, &poll, &source)?;
        poll_ids.insert(poll.id, poll_id);
    }

    for option in poll_option_rows {
        let Some(poll_id) = poll_ids.get(&option.poll_id).copied() else {
            continue;
        };
        let source = merge_source_for_child(
            input_index + 1,
            &input_path_text,
            input_conn,
            option.poll_timeline_item_id,
        )?;
        insert_poll_option_row(output_conn, poll_id, &option, &source)?;
    }

    for warning in warning_rows {
        let source = warning
            .timeline_item_id
            .map(|timeline_item_id| {
                merge_source_for_child(
                    input_index + 1,
                    &input_path_text,
                    input_conn,
                    timeline_item_id,
                )
            })
            .transpose()?;
        copy_import_warning_row(
            output_conn,
            import_id,
            &source_file_ids,
            &timeline_item_ids,
            &warning,
            source.as_ref(),
        )?;
        merge_state.warnings += 1;
        input_summary.warnings += 1;
    }

    db::finish_import(output_conn, import_id, &input_summary)?;
    Ok(())
}

fn timeline_fingerprint(
    row: &TimelineRow,
    message: Option<&MessageRow>,
    service_event: Option<&ServiceEventRow>,
    attachments: &[&AttachmentRow],
) -> Result<TimelineFingerprint> {
    let value = if row.item_kind == "message" {
        json!([
            row.item_kind,
            row.telegram_message_id,
            normalize_fingerprint_timestamp(row.timestamp.as_deref()),
            normalize_fingerprint_text(row.display_text.as_deref()),
            message.map(|message| normalize_fingerprint_text(message.plain_text.as_deref())),
            message.and_then(|message| message.reply_to_message_id),
            message.and_then(|message| message.forwarded_from.as_deref()),
            message.and_then(|message| message.forwarded_from_id.as_deref()),
            attachment_fingerprint_values(attachments),
        ])
    } else if row.item_kind == "service_event" {
        json!([
            row.item_kind,
            row.telegram_message_id,
            service_event.map(|event| event.event_type.as_str()),
            service_event.map(|event| event.target_names_json.as_str()),
        ])
    } else {
        json!([
            row.item_kind,
            row.telegram_message_id,
            normalize_fingerprint_timestamp(row.timestamp.as_deref()),
            normalize_fingerprint_text(row.display_text.as_deref()),
            row.extra_json,
        ])
    };

    Ok(TimelineFingerprint(serde_json::to_string(&value)?))
}

fn attachment_refs_by_timeline(attachments: &[AttachmentRow]) -> HashMap<i64, Vec<&AttachmentRow>> {
    let mut by_timeline: HashMap<i64, Vec<&AttachmentRow>> = HashMap::new();
    for attachment in attachments {
        by_timeline
            .entry(attachment.timeline_item_id)
            .or_default()
            .push(attachment);
    }
    by_timeline
}

fn attachment_fingerprint_values(attachments: &[&AttachmentRow]) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = attachments
        .iter()
        .map(|attachment| {
            (
                attachment.attachment_kind.clone(),
                attachment.relative_path.clone().unwrap_or_default(),
            )
        })
        .collect();
    values.sort();
    values
}

fn normalize_fingerprint_timestamp(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };

    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            timestamp
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| value.to_string())
}

fn normalize_fingerprint_text(value: Option<&str>) -> String {
    value
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn message_id_conflicts(
    seen_message_ids: &mut HashMap<i64, TimelineFingerprint>,
    row: &TimelineRow,
    fingerprint: &TimelineFingerprint,
) -> bool {
    let Some(telegram_message_id) = row.telegram_message_id else {
        return false;
    };

    match seen_message_ids.get(&telegram_message_id) {
        Some(existing) if existing != fingerprint => true,
        Some(_) => false,
        None => {
            seen_message_ids.insert(telegram_message_id, fingerprint.clone());
            false
        }
    }
}

fn read_source_files(conn: &Connection) -> Result<Vec<SourceFileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
         FROM source_files
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SourceFileRow {
            id: row.get(0)?,
            import_id: row.get(1)?,
            relative_path: row.get(2)?,
            checksum: row.get(3)?,
            file_size: row.get(4)?,
            parse_order: row.get(5)?,
            detected_chat_title: row.get(6)?,
        })
    })?;
    collect_rows(rows)
}

fn copy_source_files(
    conn: &Connection,
    import_id: i64,
    source_files: &[SourceFileRow],
) -> Result<HashMap<i64, i64>> {
    let mut ids = HashMap::new();
    for source_file in source_files {
        conn.execute(
            "INSERT INTO source_files (
                import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                import_id,
                source_file.relative_path,
                source_file.checksum,
                source_file.file_size,
                source_file.parse_order,
                source_file.detected_chat_title,
            ],
        )?;
        ids.insert(source_file.id, conn.last_insert_rowid());
    }

    Ok(ids)
}

fn read_timeline_rows(conn: &Connection) -> Result<Vec<TimelineRow>> {
    let mut stmt = conn.prepare(
        "SELECT ti.id, c.title, ti.source_file_id, ti.source_anchor,
                ti.telegram_message_id, ti.item_kind, ti.timestamp, ti.original_timestamp,
                au.display_name, ti.display_text, ti.extra_json
         FROM timeline_items ti
         JOIN chats c ON c.id = ti.chat_id
         LEFT JOIN users au ON au.id = ti.actor_user_id
         ORDER BY ti.ordinal, ti.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TimelineRow {
            id: row.get(0)?,
            chat_title: row.get(1)?,
            source_file_id: row.get(2)?,
            source_anchor: row.get(3)?,
            telegram_message_id: row.get(4)?,
            item_kind: row.get(5)?,
            timestamp: row.get(6)?,
            original_timestamp: row.get(7)?,
            actor_name: row.get(8)?,
            display_text: row.get(9)?,
            extra_json: row.get(10)?,
        })
    })?;
    collect_rows(rows)
}

fn read_message_rows(conn: &Connection) -> Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare(
        "SELECT m.timeline_item_id, m.telegram_message_id, su.display_name, m.sender_inferred,
                m.edited_timestamp, m.plain_text, m.text_entities_json, m.reply_to_message_id,
                m.reply_to_peer_id, m.forwarded_from, m.forwarded_from_id, m.forwarded_date,
                m.saved_from, m.via_bot, m.author, m.inline_bot_buttons_json, m.reactions_json,
                m.extra_json
         FROM messages m
         LEFT JOIN users su ON su.id = m.sender_user_id
         ORDER BY m.timeline_item_id, m.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MessageRow {
            timeline_item_id: row.get(0)?,
            telegram_message_id: row.get(1)?,
            sender_name: row.get(2)?,
            sender_inferred: row.get(3)?,
            edited_timestamp: row.get(4)?,
            plain_text: row.get(5)?,
            text_entities_json: row.get(6)?,
            reply_to_message_id: row.get(7)?,
            reply_to_peer_id: row.get(8)?,
            forwarded_from: row.get(9)?,
            forwarded_from_id: row.get(10)?,
            forwarded_date: row.get(11)?,
            saved_from: row.get(12)?,
            via_bot: row.get(13)?,
            author: row.get(14)?,
            inline_bot_buttons_json: row.get(15)?,
            reactions_json: row.get(16)?,
            extra_json: row.get(17)?,
        })
    })?;
    collect_rows(rows)
}

fn read_service_event_rows(conn: &Connection) -> Result<Vec<ServiceEventRow>> {
    let mut stmt = conn.prepare(
        "SELECT se.timeline_item_id, se.event_type, au.display_name, se.target_names_json,
                se.display_text, se.extra_json
         FROM service_events se
         LEFT JOIN users au ON au.id = se.actor_user_id
         ORDER BY se.timeline_item_id, se.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ServiceEventRow {
            timeline_item_id: row.get(0)?,
            event_type: row.get(1)?,
            actor_name: row.get(2)?,
            target_names_json: row.get(3)?,
            display_text: row.get(4)?,
            extra_json: row.get(5)?,
        })
    })?;
    collect_rows(rows)
}

fn read_attachment_rows(conn: &Connection) -> Result<Vec<AttachmentRow>> {
    let mut stmt = conn.prepare(
        "SELECT timeline_item_id, attachment_kind, relative_path, thumbnail_path, mime_type,
                file_size, duration_seconds, title, width, height, spoiler, ttl_seconds,
                skip_reason, extra_json
         FROM attachments
         ORDER BY timeline_item_id, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(AttachmentRow {
            timeline_item_id: row.get(0)?,
            attachment_kind: row.get(1)?,
            relative_path: row.get(2)?,
            thumbnail_path: row.get(3)?,
            mime_type: row.get(4)?,
            file_size: row.get(5)?,
            duration_seconds: row.get(6)?,
            title: row.get(7)?,
            width: row.get(8)?,
            height: row.get(9)?,
            spoiler: row.get(10)?,
            ttl_seconds: row.get(11)?,
            skip_reason: row.get(12)?,
            extra_json: row.get(13)?,
        })
    })?;
    collect_rows(rows)
}

fn read_poll_rows(conn: &Connection) -> Result<Vec<PollRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, timeline_item_id, question, closed, total_voters, extra_json
         FROM polls
         ORDER BY timeline_item_id, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PollRow {
            id: row.get(0)?,
            timeline_item_id: row.get(1)?,
            question: row.get(2)?,
            closed: row.get(3)?,
            total_voters: row.get(4)?,
            extra_json: row.get(5)?,
        })
    })?;
    collect_rows(rows)
}

fn read_poll_option_rows(conn: &Connection) -> Result<Vec<PollOptionRow>> {
    let mut stmt = conn.prepare(
        "SELECT po.poll_id, p.timeline_item_id, po.option_index, po.text, po.voters,
                po.chosen, po.extra_json
         FROM poll_options po
         JOIN polls p ON p.id = po.poll_id
         ORDER BY po.poll_id, po.option_index",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PollOptionRow {
            poll_id: row.get(0)?,
            poll_timeline_item_id: row.get(1)?,
            option_index: row.get(2)?,
            text: row.get(3)?,
            voters: row.get(4)?,
            chosen: row.get(5)?,
            extra_json: row.get(6)?,
        })
    })?;
    collect_rows(rows)
}

fn read_import_warning_rows(conn: &Connection) -> Result<Vec<ImportWarningRow>> {
    let mut stmt = conn.prepare(
        "SELECT source_file_id, timeline_item_id, severity, warning_code, message, context_json
         FROM import_warnings
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ImportWarningRow {
            source_file_id: row.get(0)?,
            timeline_item_id: row.get(1)?,
            severity: row.get(2)?,
            warning_code: row.get(3)?,
            message: row.get(4)?,
            context_json: row.get(5)?,
        })
    })?;
    collect_rows(rows)
}

fn insert_timeline_row(
    conn: &Connection,
    chat_id: i64,
    source_file_id: i64,
    ordinal: i64,
    row: &TimelineRow,
    actor_user_id: Option<i64>,
    source: &MergeSource,
) -> Result<i64> {
    let extra_json = merge_extra_json(&row.extra_json, source)?;
    conn.execute(
        "INSERT INTO timeline_items (
            chat_id, source_file_id, source_anchor, telegram_message_id, ordinal, item_kind,
            timestamp, original_timestamp, actor_user_id, display_text, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            chat_id,
            source_file_id,
            row.source_anchor,
            row.telegram_message_id,
            ordinal,
            row.item_kind,
            row.timestamp,
            row.original_timestamp,
            actor_user_id,
            row.display_text,
            extra_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_message_row(
    conn: &Connection,
    timeline_item_id: i64,
    row: &MessageRow,
    source: &MergeSource,
) -> Result<()> {
    let sender_user_id = ensure_output_user(conn, row.sender_name.as_deref())?;
    let extra_json = merge_extra_json(&row.extra_json, source)?;
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
            row.telegram_message_id,
            sender_user_id,
            row.sender_inferred,
            row.edited_timestamp,
            row.plain_text,
            row.text_entities_json,
            row.reply_to_message_id,
            row.reply_to_peer_id,
            row.forwarded_from,
            row.forwarded_from_id,
            row.forwarded_date,
            row.saved_from,
            row.via_bot,
            row.author,
            row.inline_bot_buttons_json,
            row.reactions_json,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_service_event_row(
    conn: &Connection,
    timeline_item_id: i64,
    row: &ServiceEventRow,
    source: &MergeSource,
) -> Result<()> {
    let actor_user_id = ensure_output_user(conn, row.actor_name.as_deref())?;
    let extra_json = merge_extra_json(&row.extra_json, source)?;
    conn.execute(
        "INSERT INTO service_events (
            timeline_item_id, event_type, actor_user_id, target_names_json, display_text, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            timeline_item_id,
            row.event_type,
            actor_user_id,
            row.target_names_json,
            row.display_text,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_attachment_row(
    conn: &Connection,
    timeline_item_id: i64,
    row: &AttachmentRow,
    source: &MergeSource,
) -> Result<()> {
    let extra_json = merge_extra_json(&row.extra_json, source)?;
    conn.execute(
        "INSERT INTO attachments (
            timeline_item_id, attachment_kind, relative_path, thumbnail_path, mime_type,
            file_size, duration_seconds, title, width, height, spoiler, ttl_seconds,
            skip_reason, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            timeline_item_id,
            row.attachment_kind,
            row.relative_path,
            row.thumbnail_path,
            row.mime_type,
            row.file_size,
            row.duration_seconds,
            row.title,
            row.width,
            row.height,
            row.spoiler,
            row.ttl_seconds,
            row.skip_reason,
            extra_json,
        ],
    )?;
    Ok(())
}

fn insert_poll_row(
    conn: &Connection,
    timeline_item_id: i64,
    row: &PollRow,
    source: &MergeSource,
) -> Result<i64> {
    let extra_json = merge_extra_json(&row.extra_json, source)?;
    conn.execute(
        "INSERT INTO polls (
            timeline_item_id, question, closed, total_voters, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            timeline_item_id,
            row.question,
            row.closed,
            row.total_voters,
            extra_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_poll_option_row(
    conn: &Connection,
    poll_id: i64,
    row: &PollOptionRow,
    source: &MergeSource,
) -> Result<()> {
    let extra_json = merge_extra_json(&row.extra_json, source)?;
    conn.execute(
        "INSERT INTO poll_options (
            poll_id, option_index, text, voters, chosen, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            poll_id,
            row.option_index,
            row.text,
            row.voters,
            row.chosen,
            extra_json,
        ],
    )?;
    Ok(())
}

fn copy_import_warning_row(
    conn: &Connection,
    import_id: i64,
    source_file_ids: &HashMap<i64, i64>,
    timeline_item_ids: &HashMap<i64, i64>,
    row: &ImportWarningRow,
    source: Option<&MergeSource>,
) -> Result<()> {
    let source_file_id = row
        .source_file_id
        .and_then(|source_file_id| source_file_ids.get(&source_file_id).copied());
    let timeline_item_id = row
        .timeline_item_id
        .and_then(|timeline_item_id| timeline_item_ids.get(&timeline_item_id).copied());
    let context_json = if let Some(source) = source {
        merge_extra_json(&row.context_json, source)?
    } else {
        row.context_json.clone()
    };

    conn.execute(
        "INSERT INTO import_warnings (
            import_id, source_file_id, timeline_item_id, severity, warning_code, message,
            context_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            import_id,
            source_file_id,
            timeline_item_id,
            row.severity,
            row.warning_code,
            row.message,
            context_json,
        ],
    )?;
    Ok(())
}

fn merge_source_for_child(
    input_index: usize,
    input_path: &str,
    input_conn: &Connection,
    timeline_item_id: i64,
) -> Result<MergeSource> {
    input_conn
        .query_row(
            "SELECT sf.import_id, ti.source_file_id, ti.id
             FROM timeline_items ti
             JOIN source_files sf ON sf.id = ti.source_file_id
             WHERE ti.id = ?1",
            [timeline_item_id],
            |row| {
                Ok(MergeSource {
                    input_index,
                    input_path: input_path.to_string(),
                    source_import_id: row.get(0)?,
                    source_source_file_id: row.get(1)?,
                    source_timeline_item_id: row.get(2)?,
                })
            },
        )
        .map_err(Into::into)
}

fn ensure_output_chat(conn: &Connection, import_id: i64, title: &str) -> Result<i64> {
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

fn ensure_output_user(conn: &Connection, display_name: Option<&str>) -> Result<Option<i64>> {
    let Some(display_name) = display_name.filter(|name| !name.is_empty()) else {
        return Ok(None);
    };

    conn.execute(
        "INSERT OR IGNORE INTO users (display_name) VALUES (?1)",
        [display_name],
    )?;
    Ok(Some(conn.query_row(
        "SELECT id FROM users WHERE display_name = ?1",
        [display_name],
        |row| row.get(0),
    )?))
}

fn insert_merge_warning(
    conn: &Connection,
    import_id: i64,
    timeline_item_id: Option<i64>,
    warning_code: &str,
    message: &str,
    fingerprint: &TimelineFingerprint,
    source: &MergeSource,
) -> Result<()> {
    let context_json = serde_json::to_string(&json!({
        "fingerprint": &fingerprint.0,
        "merge_source": merge_source_value(source),
    }))?;
    conn.execute(
        "INSERT INTO import_warnings (
            import_id, source_file_id, timeline_item_id, severity, warning_code, message,
            context_json
         ) VALUES (?1, NULL, ?2, 'warning', ?3, ?4, ?5)",
        params![
            import_id,
            timeline_item_id,
            warning_code,
            message,
            context_json,
        ],
    )?;
    Ok(())
}

fn merge_extra_json(existing: &str, source: &MergeSource) -> Result<String> {
    let mut value: Value = serde_json::from_str(existing)?;
    let source_value = merge_source_value(source);

    match &mut value {
        Value::Object(object) => {
            object.insert("merge_source".to_string(), source_value);
            Ok(serde_json::to_string(&value)?)
        }
        _ => Ok(serde_json::to_string(&json!({
            "source_extra_json": value,
            "merge_source": source_value,
        }))?),
    }
}

fn merge_source_value(source: &MergeSource) -> Value {
    json!({
        "input_index": source.input_index,
        "input_path": source.input_path,
        "source_import_id": source.source_import_id,
        "source_source_file_id": source.source_source_file_id,
        "source_timeline_item_id": source.source_timeline_item_id,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn open_input_database(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

fn path_identity(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize()?;
    Ok(parent.join(path.file_name().unwrap_or_default()))
}

fn temporary_database_path(output_db: &Path) -> PathBuf {
    let parent = output_db
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = output_db
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "merged.sqlite".into());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    parent.join(format!(
        ".{file_name}.tmp-{}-{timestamp}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn replace_database(temp_path: &Path, output_db: &Path) -> Result<()> {
    fs::rename(temp_path, output_db)?;
    Ok(())
}

#[cfg(not(unix))]
fn replace_database(temp_path: &Path, output_db: &Path) -> Result<()> {
    let backup_path = temporary_backup_path(output_db);
    if output_db.exists() {
        fs::rename(output_db, &backup_path)?;
    }
    if let Err(error) = fs::rename(temp_path, output_db) {
        if backup_path.exists() {
            let _ = fs::rename(&backup_path, output_db);
        }
        return Err(error.into());
    }
    cleanup_temp_database(&backup_path);
    Ok(())
}

#[cfg(not(unix))]
fn temporary_backup_path(output_db: &Path) -> PathBuf {
    let mut backup_path = temporary_database_path(output_db);
    backup_path.set_extension("backup");
    backup_path
}

fn cleanup_temp_database(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-journal", "-wal", "-shm"] {
        let _ = fs::remove_file(path_with_suffix(path, suffix));
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn path_to_db(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn parse_error(message: String) -> TelegramExportError {
    TelegramExportError::Parse(message)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;
    use crate::{db, error::TelegramExportError};

    fn options(
        output_db: std::path::PathBuf,
        input_dbs: Vec<std::path::PathBuf>,
        force: bool,
        fts: bool,
    ) -> MergeOptions {
        MergeOptions {
            output_db,
            input_dbs,
            force,
            fts,
        }
    }

    fn empty_tool_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        db::create_schema(&conn).unwrap();
    }

    fn seed_message_db(
        path: &std::path::Path,
        chat_title: &str,
        message_id: i64,
        text: &str,
        timestamp: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        db::create_schema(&conn).unwrap();
        let import_id = db::begin_import(&conn, path, path, "test").unwrap();
        conn.execute(
            "INSERT INTO source_files (
                import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
             ) VALUES (?1, ?2, ?3, 1, 0, ?4)",
            rusqlite::params![
                import_id,
                "messages.html",
                format!("checksum-{message_id}"),
                chat_title
            ],
        )
        .unwrap();
        let source_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chats (title, created_import_id) VALUES (?1, ?2)",
            rusqlite::params![chat_title, import_id],
        )
        .unwrap();
        let chat_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chat_aliases (chat_id, title) VALUES (?1, ?2)",
            rusqlite::params![chat_id, chat_title],
        )
        .unwrap();
        conn.execute("INSERT INTO users (display_name) VALUES ('Alice')", [])
            .unwrap();
        let user_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO timeline_items (
                chat_id, source_file_id, source_anchor, telegram_message_id, ordinal,
                item_kind, timestamp, original_timestamp, actor_user_id, display_text, extra_json
             ) VALUES (?1, ?2, ?3, ?4, 0, 'message', ?5, NULL, ?6, ?7, '{\"seed\":true}')",
            rusqlite::params![
                chat_id,
                source_file_id,
                format!("message{message_id}"),
                message_id,
                timestamp,
                user_id,
                text
            ],
        )
        .unwrap();
        let timeline_item_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages (
                timeline_item_id, telegram_message_id, sender_user_id, sender_inferred,
                edited_timestamp, plain_text, text_entities_json, reply_to_message_id,
                reply_to_peer_id, forwarded_from, forwarded_from_id, forwarded_date,
                saved_from, via_bot, author, inline_bot_buttons_json, reactions_json, extra_json
             ) VALUES (
                ?1, ?2, ?3, 0, NULL, ?4, '[]', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                '[]', '[]', '{\"message_seed\":true}'
             )",
            rusqlite::params![timeline_item_id, message_id, user_id, text],
        )
        .unwrap();
        db::finish_import(
            &conn,
            import_id,
            &crate::model::ImportSummary {
                files_seen: 1,
                files_imported: 1,
                timeline_items: 1,
                messages: 1,
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn seed_rich_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        db::create_schema(&conn).unwrap();
        let import_id = db::begin_import(&conn, path, path, "test").unwrap();
        conn.execute(
            "INSERT INTO source_files (
                import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
             ) VALUES (?1, 'messages.html', 'rich-checksum', 1, 0, 'Example Chat')",
            [import_id],
        )
        .unwrap();
        let source_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chats (title, created_import_id) VALUES ('Example Chat', ?1)",
            [import_id],
        )
        .unwrap();
        let chat_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chat_aliases (chat_id, title) VALUES (?1, 'Example Chat')",
            [chat_id],
        )
        .unwrap();
        conn.execute("INSERT INTO users (display_name) VALUES ('Alice')", [])
            .unwrap();
        let alice_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO users (display_name) VALUES ('Bob')", [])
            .unwrap();
        let bob_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO timeline_items (
                chat_id, source_file_id, source_anchor, telegram_message_id, ordinal,
                item_kind, timestamp, original_timestamp, actor_user_id, display_text, extra_json
             ) VALUES (
                ?1, ?2, 'message100', 100, 0, 'message', '2026-07-01T10:00:00Z',
                NULL, ?3, 'Message with media', '{\"seed\":\"message\"}'
             )",
            rusqlite::params![chat_id, source_file_id, alice_id],
        )
        .unwrap();
        let message_timeline_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO timeline_items (
                chat_id, source_file_id, source_anchor, telegram_message_id, ordinal,
                item_kind, timestamp, original_timestamp, actor_user_id, display_text, extra_json
             ) VALUES (
                ?1, ?2, 'message101', 101, 1, 'service_event', '2026-07-01T10:01:00Z',
                NULL, ?3, 'Bob pinned a message', '{\"seed\":\"service\"}'
             )",
            rusqlite::params![chat_id, source_file_id, bob_id],
        )
        .unwrap();
        let service_timeline_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages (
                timeline_item_id, telegram_message_id, sender_user_id, sender_inferred,
                edited_timestamp, plain_text, text_entities_json, reply_to_message_id,
                reply_to_peer_id, forwarded_from, forwarded_from_id, forwarded_date,
                saved_from, via_bot, author, inline_bot_buttons_json, reactions_json, extra_json
             ) VALUES (
                ?1, 100, ?2, 0, NULL, 'Message with media', '[]', NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, '[]', '[]', '{\"message_seed\":true}'
             )",
            rusqlite::params![message_timeline_id, alice_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_events (
                timeline_item_id, event_type, actor_user_id, target_names_json, display_text, extra_json
             ) VALUES (?1, 'pin_message', ?2, '[\"Alice\"]', 'Bob pinned a message', '{\"service_seed\":true}')",
            rusqlite::params![service_timeline_id, bob_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachments (
                timeline_item_id, attachment_kind, relative_path, thumbnail_path, mime_type,
                file_size, duration_seconds, title, width, height, spoiler, ttl_seconds,
                skip_reason, extra_json
             ) VALUES (
                ?1, 'file', 'files/report.pdf', NULL, 'application/pdf', 123, NULL,
                'report.pdf', NULL, NULL, 0, NULL, NULL, '{\"attachment_seed\":true}'
             )",
            [message_timeline_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO polls (timeline_item_id, question, closed, total_voters, extra_json)
             VALUES (?1, 'Pick one', 0, 3, '{\"poll_seed\":true}')",
            [message_timeline_id],
        )
        .unwrap();
        let poll_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO poll_options (poll_id, option_index, text, voters, chosen, extra_json)
             VALUES (?1, 0, 'Yes', 2, 1, '{\"option_seed\":0}')",
            [poll_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO poll_options (poll_id, option_index, text, voters, chosen, extra_json)
             VALUES (?1, 1, 'No', 1, 0, '{\"option_seed\":1}')",
            [poll_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_warnings (
                import_id, source_file_id, timeline_item_id, severity, warning_code, message,
                context_json
             ) VALUES (?1, ?2, ?3, 'warning', 'missing_attachment', 'seed warning', '{\"warning_seed\":true}')",
            rusqlite::params![import_id, source_file_id, message_timeline_id],
        )
        .unwrap();
        db::finish_import(
            &conn,
            import_id,
            &crate::model::ImportSummary {
                files_seen: 1,
                files_imported: 1,
                timeline_items: 2,
                messages: 1,
                service_events: 1,
                attachments: 1,
                warnings: 1,
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn seed_service_db(
        path: &std::path::Path,
        message_id: i64,
        timestamp: Option<&str>,
        display_text: &str,
        event_type: &str,
        target_names_json: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        db::create_schema(&conn).unwrap();
        let import_id = db::begin_import(&conn, path, path, "test").unwrap();
        conn.execute(
            "INSERT INTO source_files (
                import_id, relative_path, checksum, file_size, parse_order, detected_chat_title
             ) VALUES (?1, 'messages.html', ?2, 1, 0, 'Example Chat')",
            rusqlite::params![import_id, format!("service-checksum-{message_id}")],
        )
        .unwrap();
        let source_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chats (title, created_import_id) VALUES ('Example Chat', ?1)",
            [import_id],
        )
        .unwrap();
        let chat_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chat_aliases (chat_id, title) VALUES (?1, 'Example Chat')",
            [chat_id],
        )
        .unwrap();
        conn.execute("INSERT INTO users (display_name) VALUES ('Alice')", [])
            .unwrap();
        let actor_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO timeline_items (
                chat_id, source_file_id, source_anchor, telegram_message_id, ordinal,
                item_kind, timestamp, original_timestamp, actor_user_id, display_text, extra_json
             ) VALUES (?1, ?2, ?3, ?4, 0, 'service_event', ?5, NULL, ?6, ?7, '{}')",
            rusqlite::params![
                chat_id,
                source_file_id,
                format!("message{message_id}"),
                message_id,
                timestamp,
                actor_id,
                display_text,
            ],
        )
        .unwrap();
        let timeline_item_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO service_events (
                timeline_item_id, event_type, actor_user_id, target_names_json, display_text, extra_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, '{}')",
            rusqlite::params![
                timeline_item_id,
                event_type,
                actor_id,
                target_names_json,
                display_text,
            ],
        )
        .unwrap();
        db::finish_import(
            &conn,
            import_id,
            &crate::model::ImportSummary {
                files_seen: 1,
                files_imported: 1,
                timeline_items: 1,
                service_events: 1,
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn refuses_merge_without_inputs() {
        let temp = tempdir().unwrap();
        let output = temp.path().join("merged.sqlite");

        let error = run_merge(options(output, vec![], false, false)).unwrap_err();

        assert!(matches!(error, TelegramExportError::MergeRequiresInput));
    }

    #[test]
    fn refuses_existing_output_without_force() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("input.sqlite");
        let output = temp.path().join("merged.sqlite");
        empty_tool_db(&input);
        fs::write(&output, "existing").unwrap();

        let error = run_merge(options(output.clone(), vec![input], false, false)).unwrap_err();

        assert!(matches!(
            error,
            TelegramExportError::OutputDatabaseExists(path) if path == output
        ));
    }

    #[test]
    fn refuses_output_that_is_also_input() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("same.sqlite");
        empty_tool_db(&db_path);

        let error =
            run_merge(options(db_path.clone(), vec![db_path.clone()], true, false)).unwrap_err();

        assert!(matches!(
            error,
            TelegramExportError::MergeOutputIsInput { output, input }
                if output == db_path && input == db_path
        ));
    }

    #[test]
    fn refuses_unsupported_schema_version() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("bad.sqlite");
        let output = temp.path().join("merged.sqlite");
        let conn = Connection::open(&input).unwrap();
        conn.pragma_update(None, "user_version", 999_i64).unwrap();

        let error = run_merge(options(output, vec![input.clone()], true, false)).unwrap_err();

        assert!(matches!(
            error,
            TelegramExportError::UnsupportedSchemaVersion { path, version }
                if path == input && version == 999
        ));
    }

    #[test]
    fn merges_input_databases_as_contiguous_timeline() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.sqlite");
        let second = temp.path().join("second.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_message_db(&first, "Example Chat", 1, "First", "2026-07-01T10:00:00Z");
        seed_message_db(&second, "Example Chat", 2, "Second", "2026-07-01T10:01:00Z");

        let summary = run_merge(options(output.clone(), vec![first, second], true, false)).unwrap();

        assert_eq!(summary.input_databases, 2);
        assert_eq!(summary.timeline_items, 2);
        assert_eq!(summary.messages, 2);
        let conn = Connection::open(output).unwrap();
        let rows: Vec<(i64, i64, String)> = conn
            .prepare(
                "SELECT ti.ordinal, m.telegram_message_id, ti.display_text
                 FROM timeline_items ti
                 JOIN messages m ON m.timeline_item_id = ti.id
                 ORDER BY ti.ordinal",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![(0, 1, "First".to_string()), (1, 2, "Second".to_string()),]
        );
    }

    #[test]
    fn skips_exact_duplicate_timeline_rows() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.sqlite");
        let second = temp.path().join("second.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_message_db(
            &first,
            "Example Chat",
            10,
            "Same text",
            "2026-07-01T10:00:00Z",
        );
        seed_message_db(
            &second,
            "Example Chat",
            10,
            "Same   text",
            "2026-07-01T10:00:00Z",
        );

        let summary = run_merge(options(output.clone(), vec![first, second], true, false)).unwrap();

        assert_eq!(summary.timeline_items, 1);
        assert_eq!(summary.duplicates_skipped, 1);
        assert_eq!(summary.warnings, 1);
        let conn = Connection::open(output).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM timeline_items", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'merge_duplicate_skipped'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn skips_duplicate_messages_with_equivalent_rfc3339_timestamps() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.sqlite");
        let second = temp.path().join("second.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_message_db(
            &first,
            "Example Chat",
            42,
            "Same text",
            "2026-07-01T10:00:00Z",
        );
        seed_message_db(
            &second,
            "Example Chat",
            42,
            "Same text",
            "2026-07-01T10:00:00+00:00",
        );

        let summary = run_merge(options(output, vec![first, second], true, false)).unwrap();

        assert_eq!(summary.timeline_items, 1);
        assert_eq!(summary.duplicates_skipped, 1);
        assert_eq!(summary.conflicts_kept, 0);
    }

    #[test]
    fn skips_duplicate_service_events_with_exporter_specific_display_text() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.sqlite");
        let second = temp.path().join("second.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_service_db(
            &first,
            77,
            None,
            "Alice invited Bob",
            "invite_members",
            "[\"Bob\"]",
        );
        seed_service_db(
            &second,
            77,
            Some("2026-07-01T10:00:00+00:00"),
            "Alice invite members: Bob",
            "invite_members",
            "[\"Bob\"]",
        );

        let summary = run_merge(options(output, vec![first, second], true, false)).unwrap();

        assert_eq!(summary.timeline_items, 1);
        assert_eq!(summary.duplicates_skipped, 1);
        assert_eq!(summary.conflicts_kept, 0);
    }

    #[test]
    fn keeps_same_message_id_conflicts() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.sqlite");
        let second = temp.path().join("second.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_message_db(
            &first,
            "Example Chat",
            10,
            "Original",
            "2026-07-01T10:00:00Z",
        );
        seed_message_db(
            &second,
            "Example Chat",
            10,
            "Different",
            "2026-07-01T10:00:00Z",
        );

        let summary = run_merge(options(output.clone(), vec![first, second], true, false)).unwrap();

        assert_eq!(summary.timeline_items, 2);
        assert_eq!(summary.conflicts_kept, 1);
        assert_eq!(summary.warnings, 1);
        let conn = Connection::open(output).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'merge_message_id_conflict'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn copies_child_tables_with_remapped_foreign_keys() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("input.sqlite");
        let output = temp.path().join("merged.sqlite");
        seed_rich_db(&input);

        let summary = run_merge(options(output.clone(), vec![input], true, false)).unwrap();

        assert_eq!(summary.timeline_items, 2);
        assert_eq!(summary.messages, 1);
        assert_eq!(summary.service_events, 1);
        assert_eq!(summary.attachments, 1);
        assert_eq!(summary.warnings, 1);
        let conn = Connection::open(output).unwrap();
        for table in [
            "service_events",
            "attachments",
            "polls",
            "poll_options",
            "import_warnings",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert!(count > 0, "expected rows in {table}");
        }
        let orphan_attachments: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachments a
                 LEFT JOIN timeline_items ti ON ti.id = a.timeline_item_id
                 WHERE ti.id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_attachments, 0);
    }
}
