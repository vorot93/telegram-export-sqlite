use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, TelegramExportError>;

#[derive(Debug, thiserror::Error)]
pub enum TelegramExportError {
    #[error("input directory does not exist: {0}")]
    InputDirectoryMissing(PathBuf),

    #[error("no messages*.html files found under: {0}")]
    NoMessagesFiles(PathBuf),

    #[error("output database already exists: {0}; pass --force or --incremental")]
    OutputDatabaseExists(PathBuf),

    #[error("merge output database already exists: {0}; pass --force")]
    MergeOutputExists(PathBuf),

    #[error("incremental import requires an existing database: {0}")]
    IncrementalDatabaseMissing(PathBuf),

    #[error(
        "incremental refresh targets a different chat: export chat \"{incoming}\" does not match the archived chat \"{existing}\"; re-import fresh for a different chat"
    )]
    IncrementalChatMismatch { existing: String, incoming: String },

    #[error("merge requires at least one input database")]
    MergeRequiresInput,

    #[error(
        "merge output database must not also be an input database: output {output}, input {input}"
    )]
    MergeOutputIsInput { output: PathBuf, input: PathBuf },

    #[error("unsupported SQLite schema version in {path}: {version}")]
    UnsupportedSchemaVersion { path: PathBuf, version: i64 },

    #[error("input database is missing required table {table} in {path}")]
    MissingRequiredTable { path: PathBuf, table: &'static str },

    #[error("input database does not exist: {0}")]
    InputDatabaseMissing(PathBuf),

    #[error("output directory already exists: {0}; pass --force")]
    OutputDirectoryExists(PathBuf),

    #[error("output path is a file, expected directory: {0}")]
    OutputPathIsFile(PathBuf),

    #[error("input database must not be inside output directory: input {input}, output {output}")]
    ExportInputInsideOutput { input: PathBuf, output: PathBuf },

    #[error(
        "bundle destination must not overlap the export directory: dest {dest}, export {export}"
    )]
    BundleDestOverlapsExport { dest: PathBuf, export: PathBuf },

    #[error(
        "multi-chat export not supported: this looks like a full-account export containing {chats} chats; this tool archives one chat per database. Re-export the individual chat you want to archive."
    )]
    MultiChatExportNotSupported { chats: usize },

    #[error("failed to parse Telegram export: {0}")]
    Parse(String),

    #[error("output file already exists: {0}; pass --force")]
    OutputFileExists(PathBuf),

    #[error("export output file must not be the input database: {0}")]
    ExportOutputIsInputDatabase(PathBuf),

    #[error("invalid --transcribe command: {0}")]
    TranscribeCommandInvalid(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
