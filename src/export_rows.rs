use crate::{
    db,
    error::{Result, TelegramExportError},
};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub const REQUIRED_TABLES: &[&str] = &[
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
pub struct ExportRows {
    pub chat_title: String,
    pub timeline_items: Vec<TimelineRow>,
    pub messages: Vec<MessageRow>,
    pub service_events: Vec<ServiceEventRow>,
    pub attachments: Vec<AttachmentRow>,
    pub polls: Vec<PollRow>,
    pub poll_options: Vec<PollOptionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    pub id: i64,
    pub ordinal: i64,
    pub item_kind: String,
    pub source_anchor: Option<String>,
    pub telegram_message_id: Option<i64>,
    pub timestamp: Option<String>,
    pub original_timestamp: Option<String>,
    pub actor_name: Option<String>,
    pub display_text: Option<String>,
    pub extra_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub timeline_item_id: i64,
    pub telegram_message_id: i64,
    pub sender_name: Option<String>,
    pub sender_inferred: bool,
    pub edited_timestamp: Option<String>,
    pub plain_text: Option<String>,
    pub text_entities_json: String,
    pub reply_to_message_id: Option<i64>,
    pub reply_to_peer_id: Option<String>,
    pub forwarded_from: Option<String>,
    pub forwarded_from_id: Option<String>,
    pub forwarded_date: Option<String>,
    pub saved_from: Option<String>,
    pub via_bot: Option<String>,
    pub author: Option<String>,
    pub inline_bot_buttons_json: String,
    pub reactions_json: String,
    pub extra_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEventRow {
    pub timeline_item_id: i64,
    pub event_type: String,
    pub actor_name: Option<String>,
    pub target_names_json: String,
    pub display_text: String,
    pub extra_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub timeline_item_id: i64,
    pub attachment_kind: String,
    pub relative_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub title: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub spoiler: bool,
    pub ttl_seconds: Option<i64>,
    pub skip_reason: Option<String>,
    pub extra_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollRow {
    pub id: i64,
    pub timeline_item_id: i64,
    pub question: String,
    pub closed: Option<bool>,
    pub total_voters: Option<i64>,
    pub extra_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOptionRow {
    pub poll_id: i64,
    pub option_index: i64,
    pub text: String,
    pub voters: Option<i64>,
    pub chosen: Option<bool>,
    pub extra_json: String,
}

pub fn validate_input_database(conn: &Connection, path: &Path) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != db::SCHEMA_VERSION {
        return Err(TelegramExportError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            version,
        });
    }

    for &table in REQUIRED_TABLES {
        let exists: i64 = conn.query_row(
            "SELECT EXISTS (
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table'
                  AND name = ?1
            )",
            [table],
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

pub fn load_export(conn: &Connection) -> Result<ExportRows> {
    Ok(ExportRows {
        chat_title: load_chat_title(conn)?,
        timeline_items: load_timeline_rows(conn)?,
        messages: load_message_rows(conn)?,
        service_events: load_service_event_rows(conn)?,
        attachments: load_attachment_rows(conn)?,
        polls: load_poll_rows(conn)?,
        poll_options: load_poll_option_rows(conn)?,
    })
}

fn load_chat_title(conn: &Connection) -> Result<String> {
    Ok(conn
        .query_row("SELECT title FROM chats ORDER BY id LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or_else(|| "Telegram Export".to_string()))
}

fn load_timeline_rows(conn: &Connection) -> Result<Vec<TimelineRow>> {
    let mut stmt = conn.prepare(
        "SELECT ti.id, ti.ordinal, ti.item_kind, ti.source_anchor,
                ti.telegram_message_id, ti.timestamp, ti.original_timestamp,
                u.display_name, ti.display_text, ti.extra_json
         FROM timeline_items ti
         LEFT JOIN users u ON u.id = ti.actor_user_id
         ORDER BY ti.ordinal, ti.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TimelineRow {
            id: row.get(0)?,
            ordinal: row.get(1)?,
            item_kind: row.get(2)?,
            source_anchor: row.get(3)?,
            telegram_message_id: row.get(4)?,
            timestamp: row.get(5)?,
            original_timestamp: row.get(6)?,
            actor_name: row.get(7)?,
            display_text: row.get(8)?,
            extra_json: row.get(9)?,
        })
    })?;
    collect_rows(rows)
}

fn load_message_rows(conn: &Connection) -> Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare(
        "SELECT m.timeline_item_id, m.telegram_message_id, u.display_name,
                m.sender_inferred, m.edited_timestamp, m.plain_text,
                m.text_entities_json, m.reply_to_message_id, m.reply_to_peer_id,
                m.forwarded_from, m.forwarded_from_id, m.forwarded_date,
                m.saved_from, m.via_bot, m.author, m.inline_bot_buttons_json,
                m.reactions_json, m.extra_json
         FROM messages m
         LEFT JOIN users u ON u.id = m.sender_user_id
         ORDER BY m.timeline_item_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MessageRow {
            timeline_item_id: row.get(0)?,
            telegram_message_id: row.get(1)?,
            sender_name: row.get(2)?,
            sender_inferred: int_to_bool(row.get(3)?),
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

fn load_service_event_rows(conn: &Connection) -> Result<Vec<ServiceEventRow>> {
    let mut stmt = conn.prepare(
        "SELECT se.timeline_item_id, se.event_type, u.display_name,
                se.target_names_json, se.display_text, se.extra_json
         FROM service_events se
         LEFT JOIN users u ON u.id = se.actor_user_id
         ORDER BY se.timeline_item_id",
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

fn load_attachment_rows(conn: &Connection) -> Result<Vec<AttachmentRow>> {
    let mut stmt = conn.prepare(
        "SELECT timeline_item_id, attachment_kind, relative_path,
                thumbnail_path, mime_type, file_size, duration_seconds,
                title, width, height, spoiler, ttl_seconds, skip_reason,
                extra_json
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
            spoiler: int_to_bool(row.get(10)?),
            ttl_seconds: row.get(11)?,
            skip_reason: row.get(12)?,
            extra_json: row.get(13)?,
        })
    })?;
    collect_rows(rows)
}

fn load_poll_rows(conn: &Connection) -> Result<Vec<PollRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, timeline_item_id, question, closed, total_voters,
                extra_json
         FROM polls
         ORDER BY timeline_item_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PollRow {
            id: row.get(0)?,
            timeline_item_id: row.get(1)?,
            question: row.get(2)?,
            closed: option_int_to_bool(row.get(3)?),
            total_voters: row.get(4)?,
            extra_json: row.get(5)?,
        })
    })?;
    collect_rows(rows)
}

fn load_poll_option_rows(conn: &Connection) -> Result<Vec<PollOptionRow>> {
    let mut stmt = conn.prepare(
        "SELECT poll_id, option_index, text, voters, chosen, extra_json
         FROM poll_options
         ORDER BY poll_id, option_index",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PollOptionRow {
            poll_id: row.get(0)?,
            option_index: row.get(1)?,
            text: row.get(2)?,
            voters: row.get(3)?,
            chosen: option_int_to_bool(row.get(4)?),
            extra_json: row.get(5)?,
        })
    })?;
    collect_rows(rows)
}

fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn int_to_bool(value: i64) -> bool {
    value != 0
}

fn option_int_to_bool(value: Option<i64>) -> Option<bool> {
    value.map(int_to_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db,
        error::TelegramExportError,
        model::{
            Attachment, Chat, ImportSummary, Message, ParsedExport, Poll, PollOption, ServiceEvent,
            SourceFile, TextEntity, TextEntityKind, TimelineItem, TimelineItemKind,
        },
    };
    use rusqlite::Connection;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    #[test]
    fn validates_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();

        let err = validate_input_database(&conn, Path::new("bad.sqlite")).unwrap_err();

        match err {
            TelegramExportError::UnsupportedSchemaVersion { path, version } => {
                assert_eq!(path, PathBuf::from("bad.sqlite"));
                assert_eq!(version, 999);
            }
            other => panic!("expected unsupported schema version, got {other:?}"),
        }
    }

    #[test]
    fn loads_export_rows_in_timeline_order() {
        let conn = Connection::open_in_memory().unwrap();
        db::create_schema(&conn).unwrap();
        let import_id =
            db::begin_import(&conn, "fixtures/family", "family.sqlite", "full").unwrap();
        let source = SourceFile {
            absolute_path: PathBuf::from("/tmp/export/messages.html"),
            relative_path: PathBuf::from("messages.html"),
            checksum: "checksum-family".to_string(),
            file_size: 1234,
            parse_order: 0,
        };
        let parsed = ParsedExport {
            chat: Some(Chat {
                title: "Family Chat".to_string(),
            }),
            timeline_items: vec![
                TimelineItem {
                    id: None,
                    source_file_parse_order: 0,
                    source_anchor: Some("message-101".to_string()),
                    telegram_message_id: Some(101),
                    ordinal: 3,
                    kind: TimelineItemKind::Message,
                    timestamp: Some("2026-06-01T12:00:00Z".to_string()),
                    original_timestamp: Some("1 June 2026, 12:00".to_string()),
                    actor_name: Some("Alice".to_string()),
                    display_text: Some("Hello bold".to_string()),
                    extra_json: json!({}),
                },
                TimelineItem {
                    id: None,
                    source_file_parse_order: 0,
                    source_anchor: Some("message-202".to_string()),
                    telegram_message_id: Some(202),
                    ordinal: 4,
                    kind: TimelineItemKind::ServiceEvent,
                    timestamp: Some("2026-06-01T12:05:00Z".to_string()),
                    original_timestamp: Some("1 June 2026, 12:05".to_string()),
                    actor_name: Some("Carol".to_string()),
                    display_text: Some("Carol invited Bob".to_string()),
                    extra_json: json!({}),
                },
            ],
            messages: vec![Message {
                timeline_ordinal: 3,
                telegram_message_id: 101,
                sender_name: Some("Alice".to_string()),
                sender_inferred: false,
                edited_timestamp: None,
                plain_text: Some("Hello bold".to_string()),
                text_entities: vec![TextEntity {
                    kind: TextEntityKind::Bold,
                    text: "bold".to_string(),
                    extra: json!({}),
                }],
                reply_to_message_id: None,
                reply_to_peer_id: None,
                forwarded_from: None,
                forwarded_from_id: None,
                forwarded_date: None,
                saved_from: None,
                via_bot: None,
                author: None,
                inline_bot_buttons: json!([]),
                reactions: json!([]),
                extra_json: json!({}),
            }],
            service_events: vec![ServiceEvent {
                timeline_ordinal: 4,
                event_type: "invite_members".to_string(),
                actor_name: Some("Carol".to_string()),
                target_names: vec!["Bob".to_string()],
                display_text: "Carol invited Bob".to_string(),
                extra_json: json!({}),
            }],
            attachments: vec![Attachment {
                timeline_ordinal: 3,
                kind: "photo".to_string(),
                relative_path: Some(PathBuf::from("photos/family.jpg")),
                thumbnail_path: Some(PathBuf::from("photos/family_thumb.jpg")),
                mime_type: Some("image/jpeg".to_string()),
                file_size: Some(4096),
                duration_seconds: None,
                title: Some("family.jpg".to_string()),
                width: Some(800),
                height: Some(600),
                spoiler: true,
                ttl_seconds: None,
                skip_reason: None,
                extra_json: json!({}),
            }],
            polls: vec![Poll {
                timeline_ordinal: 3,
                question: "Dinner?".to_string(),
                closed: Some(true),
                total_voters: Some(3),
                extra_json: json!({}),
            }],
            poll_options: vec![PollOption {
                timeline_ordinal: 3,
                option_index: 0,
                text: "Pizza".to_string(),
                voters: Some(2),
                chosen: Some(true),
                extra_json: json!({}),
            }],
            ..ParsedExport::default()
        };
        db::insert_parsed_export(&conn, import_id, std::slice::from_ref(&source), &parsed).unwrap();
        db::finish_import(
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
                warnings: 0,
            },
        )
        .unwrap();

        let export = load_export(&conn).unwrap();

        assert_eq!(export.chat_title, "Family Chat");
        assert_eq!(export.timeline_items.len(), 2);
        assert_eq!(export.timeline_items[0].ordinal, 3);
        assert_eq!(export.timeline_items[1].ordinal, 4);
        assert_eq!(export.messages[0].telegram_message_id, 101);
        assert_eq!(export.messages[0].sender_name.as_deref(), Some("Alice"));
        assert!(export.messages[0].text_entities_json.contains("\"bold\""));
        assert_eq!(export.service_events.len(), 1);
        assert_eq!(export.service_events[0].event_type, "invite_members");
        assert_eq!(export.service_events[0].display_text, "Carol invited Bob");
        assert_eq!(
            export.service_events[0].actor_name.as_deref(),
            Some("Carol")
        );
        assert!(
            export.service_events[0]
                .target_names_json
                .contains("\"Bob\"")
        );
        assert_eq!(export.attachments.len(), 1);
        assert_eq!(export.attachments[0].attachment_kind, "photo");
        assert_eq!(
            export.attachments[0].relative_path.as_deref(),
            Some("photos/family.jpg")
        );
        assert_eq!(export.attachments[0].title.as_deref(), Some("family.jpg"));
        assert_eq!(export.attachments[0].file_size, Some(4096));
        assert!(export.attachments[0].spoiler);
        assert_eq!(export.polls.len(), 1);
        assert_eq!(export.polls[0].question, "Dinner?");
        assert_eq!(export.polls[0].closed, Some(true));
        assert_eq!(export.polls[0].total_voters, Some(3));
        assert_eq!(export.poll_options.len(), 1);
        assert_eq!(export.poll_options[0].poll_id, export.polls[0].id);
        assert_eq!(export.poll_options[0].option_index, 0);
        assert_eq!(export.poll_options[0].text, "Pizza");
        assert_eq!(export.poll_options[0].voters, Some(2));
        assert_eq!(export.poll_options[0].chosen, Some(true));
    }
}
