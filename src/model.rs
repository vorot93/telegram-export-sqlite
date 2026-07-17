use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub input_dir: PathBuf,
    pub dest: Option<PathBuf>,
    pub force: bool,
    pub incremental: bool,
    pub fts: bool,
}

#[derive(Debug, Clone)]
pub struct MergeOptions {
    pub output_db: PathBuf,
    pub input_dbs: Vec<PathBuf>,
    pub force: bool,
    pub fts: bool,
}

#[derive(Debug, Clone)]
pub struct ExportHtmlOptions {
    pub input_db: PathBuf,
    pub output_dir: PathBuf,
    pub force: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub files_seen: usize,
    pub files_imported: usize,
    pub files_skipped: usize,
    pub timeline_items: usize,
    pub messages: usize,
    pub service_events: usize,
    pub attachments: usize,
    pub warnings: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MergeSummary {
    pub input_databases: usize,
    pub timeline_items: usize,
    pub messages: usize,
    pub service_events: usize,
    pub attachments: usize,
    pub duplicates_skipped: usize,
    pub conflicts_kept: usize,
    pub warnings: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportHtmlSummary {
    pub timeline_items: usize,
    pub messages: usize,
    pub service_events: usize,
    pub attachments: usize,
    pub polls: usize,
    pub generated_date_separators: usize,
    pub media_files_copied: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub absolute_path: PathBuf,
    pub relative_path: PathBuf,
    pub checksum: String,
    pub file_size: u64,
    pub parse_order: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineItemKind {
    Message,
    ServiceEvent,
    Unsupported,
    DateSeparator,
}

#[derive(Debug, Clone)]
pub struct TimelineItem {
    pub id: Option<i64>,
    pub source_file_parse_order: usize,
    pub source_anchor: Option<String>,
    pub telegram_message_id: Option<i64>,
    pub ordinal: i64,
    pub kind: TimelineItemKind,
    pub timestamp: Option<String>,
    pub original_timestamp: Option<String>,
    pub actor_name: Option<String>,
    /// Telegram's stable actor id (sender for messages, actor for service events)
    /// when the source provides one (JSON exports); `None` for HTML exports.
    pub actor_id: Option<String>,
    pub display_text: Option<String>,
    pub extra_json: Value,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub timeline_ordinal: i64,
    /// The Telegram message id when the source carries one; `None` when it does
    /// not (rather than a fabricated stand-in). Absence is represented once.
    pub telegram_message_id: Option<i64>,
    pub sender_name: Option<String>,
    /// Telegram's stable sender id (e.g. `user12345`) when the source provides one
    /// (JSON exports). `None` for HTML exports, which carry only a display name.
    pub sender_id: Option<String>,
    pub sender_inferred: bool,
    pub edited_timestamp: Option<String>,
    pub plain_text: Option<String>,
    pub text_entities: Vec<TextEntity>,
    pub reply_to_message_id: Option<i64>,
    pub reply_to_peer_id: Option<String>,
    pub forwarded_from: Option<String>,
    pub forwarded_from_id: Option<String>,
    pub forwarded_date: Option<String>,
    pub saved_from: Option<String>,
    pub via_bot: Option<String>,
    pub author: Option<String>,
    pub inline_bot_buttons: Value,
    pub reactions: Value,
    pub extra_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEntity {
    #[serde(rename = "type")]
    pub kind: TextEntityKind,
    pub text: String,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEntityKind {
    #[serde(rename = "plain")]
    Text,
    Unknown,
    Mention,
    Hashtag,
    BotCommand,
    #[serde(rename = "link")]
    Url,
    Email,
    Bold,
    Italic,
    Code,
    Pre,
    #[serde(rename = "text_link")]
    TextUrl,
    MentionName,
    Phone,
    Cashtag,
    Underline,
    #[serde(rename = "strikethrough")]
    Strike,
    Blockquote,
    BankCard,
    Spoiler,
    CustomEmoji,
}

#[derive(Debug, Clone)]
pub struct ServiceEvent {
    pub timeline_ordinal: i64,
    pub event_type: String,
    pub actor_name: Option<String>,
    pub target_names: Vec<String>,
    pub display_text: String,
    pub extra_json: Value,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub timeline_ordinal: i64,
    pub kind: String,
    pub relative_path: Option<PathBuf>,
    pub thumbnail_path: Option<PathBuf>,
    pub mime_type: Option<String>,
    pub file_size: Option<u64>,
    pub duration_seconds: Option<i64>,
    pub title: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub spoiler: bool,
    pub ttl_seconds: Option<i64>,
    pub skip_reason: Option<String>,
    pub extra_json: Value,
}

#[derive(Debug, Clone)]
pub struct Poll {
    pub timeline_ordinal: i64,
    pub question: String,
    pub closed: Option<bool>,
    pub total_voters: Option<i64>,
    pub extra_json: Value,
}

#[derive(Debug, Clone)]
pub struct PollOption {
    pub timeline_ordinal: i64,
    pub option_index: i64,
    pub text: String,
    pub voters: Option<i64>,
    pub chosen: Option<bool>,
    pub extra_json: Value,
}

#[derive(Debug, Clone)]
pub struct ImportWarning {
    pub source_file_parse_order: Option<usize>,
    pub timeline_ordinal: Option<i64>,
    pub code: WarningCode,
    pub message: String,
    pub context: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningCode {
    UnknownServiceEvent,
    MissingAttachment,
    UnsupportedMediaShape,
    MalformedTimestamp,
    InferredSender,
    ExtraJsonOnly,
}

impl WarningCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnknownServiceEvent => "unknown_service_event",
            Self::MissingAttachment => "missing_attachment",
            Self::UnsupportedMediaShape => "unsupported_media_shape",
            Self::MalformedTimestamp => "malformed_timestamp",
            Self::InferredSender => "inferred_sender",
            Self::ExtraJsonOnly => "extra_json_only",
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ParsedExport {
    pub chat: Option<Chat>,
    pub timeline_items: Vec<TimelineItem>,
    pub messages: Vec<Message>,
    pub service_events: Vec<ServiceEvent>,
    pub attachments: Vec<Attachment>,
    pub polls: Vec<Poll>,
    pub poll_options: Vec<PollOption>,
    pub warnings: Vec<ImportWarning>,
    /// SHA-256 (hex) of the exact source bytes this parse read. The importer
    /// stores this as the source file's checksum so it always matches the
    /// imported content, closing the discovery-hash vs. parse-read TOCTOU.
    pub source_checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputTarget {
    File(PathBuf),
    Stdout,
}

#[derive(Debug, Clone)]
pub struct ExportLlmOptions {
    pub input_db: PathBuf,
    pub output: OutputTarget,
    pub force: bool,
    /// Raw `--transcribe` command; `None` disables transcription.
    pub transcribe: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportLlmSummary {
    pub messages: usize,
    pub service_events: usize,
    pub attachments: usize,
    pub polls: usize,
    pub participants: usize,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
    pub output_bytes: usize,
    pub estimated_tokens: usize,
    pub transcribed: usize,
    pub transcribe_failed: usize,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_entity_serializes_with_telegram_vocabulary() {
        let entity = TextEntity {
            kind: TextEntityKind::TextUrl,
            text: "OpenAI".to_string(),
            extra: json!({ "href": "https://openai.com" }),
        };

        assert_eq!(serde_json::to_value(entity).unwrap()["type"], "text_link");
    }

    #[test]
    fn warning_codes_are_stable_strings() {
        assert_eq!(
            WarningCode::UnknownServiceEvent.as_str(),
            "unknown_service_event"
        );
        assert_eq!(WarningCode::InferredSender.as_str(), "inferred_sender");
    }
}
