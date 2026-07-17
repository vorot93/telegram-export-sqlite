use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "telegram-export-sqlite")]
#[command(
    about = "Import Telegram Desktop exports to SQLite and export SQLite to HTML or LLM Markdown"
)]
#[command(
    after_help = "Usage:\n  telegram-export-sqlite import <EXPORT_DIR> [DEST]\n  telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>...\n  telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR>\n  telegram-export-sqlite export-llm <INPUT_DB> <OUTPUT_FILE>"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import a Telegram Desktop HTML or JSON export directory into SQLite.
    Import {
        #[arg(value_name = "EXPORT_DIR")]
        input_dir: PathBuf,
        dest: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        incremental: bool,
        #[arg(long)]
        fts: bool,
    },
    /// Merge importer-created SQLite databases as continuation chunks.
    Merge {
        output_db: PathBuf,
        input_dbs: Vec<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        fts: bool,
    },
    /// Export an importer-created SQLite database to Telegram Desktop-style HTML.
    ExportHtml {
        input_db: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Export an importer-created SQLite database to compact, LLM-optimized Markdown.
    ExportLlm {
        input_db: PathBuf,
        #[arg(value_name = "OUTPUT_FILE")]
        output_file: PathBuf,
        #[arg(long)]
        force: bool,
        /// Transcribe voice notes and round video notes with an external command.
        /// Each argument equal to `{}` is replaced by the audio file path (else
        /// the path is appended). Runs directly, no shell.
        #[arg(long, value_name = "COMMAND")]
        transcribe: Option<String>,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Import {
            input_dir,
            dest,
            force,
            incremental,
            fts,
        } => {
            let options = crate::model::ImportOptions {
                input_dir,
                dest,
                force,
                incremental,
                fts,
            };
            let summary = crate::importer::run_import(options)?;
            crate::llm_export::write_stdout(&format!(
                "files seen: {}\nfiles imported: {}\nfiles skipped: {}\ntimeline items: {}\n\
                 messages: {}\nservice events: {}\nattachments: {}\nwarnings: {}\n",
                summary.files_seen,
                summary.files_imported,
                summary.files_skipped,
                summary.timeline_items,
                summary.messages,
                summary.service_events,
                summary.attachments,
                summary.warnings,
            ))?;
        }
        Command::Merge {
            output_db,
            input_dbs,
            force,
            fts,
        } => {
            let options = crate::model::MergeOptions {
                output_db,
                input_dbs,
                force,
                fts,
            };
            let summary = crate::merge::run_merge(options)?;
            crate::llm_export::write_stdout(&format!(
                "input databases: {}\ntimeline items: {}\nmessages: {}\nservice events: {}\n\
                 attachments: {}\nduplicates skipped: {}\nconflicts kept: {}\nwarnings: {}\n",
                summary.input_databases,
                summary.timeline_items,
                summary.messages,
                summary.service_events,
                summary.attachments,
                summary.duplicates_skipped,
                summary.conflicts_kept,
                summary.warnings,
            ))?;
        }
        Command::ExportHtml {
            input_db,
            output_dir,
            force,
        } => {
            let options = crate::model::ExportHtmlOptions {
                input_db,
                output_dir,
                force,
            };
            let summary = crate::html_export::run_export_html(options)?;
            crate::llm_export::write_stdout(&format!(
                "timeline items: {}\nmessages: {}\nservice events: {}\nattachments: {}\n\
                 polls: {}\ngenerated date separators: {}\nmedia files copied: {}\n",
                summary.timeline_items,
                summary.messages,
                summary.service_events,
                summary.attachments,
                summary.polls,
                summary.generated_date_separators,
                summary.media_files_copied,
            ))?;
        }
        Command::ExportLlm {
            input_db,
            output_file,
            force,
            transcribe,
        } => {
            let output = if output_file.as_os_str() == "-" {
                crate::model::OutputTarget::Stdout
            } else {
                crate::model::OutputTarget::File(output_file)
            };
            let transcribe_enabled = transcribe.is_some();
            let options = crate::model::ExportLlmOptions {
                input_db,
                output,
                force,
                transcribe,
            };
            let summary = crate::llm_export::run_export_llm(options)?;
            eprintln!("messages: {}", summary.messages);
            eprintln!("service events: {}", summary.service_events);
            eprintln!("attachments: {}", summary.attachments);
            eprintln!("polls: {}", summary.polls);
            eprintln!("participants: {}", summary.participants);
            eprintln!(
                "date range: {} → {}",
                summary.first_date.as_deref().unwrap_or("?"),
                summary.last_date.as_deref().unwrap_or("?"),
            );
            eprintln!("output bytes: {}", summary.output_bytes);
            eprintln!("estimated tokens: {}", summary.estimated_tokens);
            if transcribe_enabled {
                eprintln!(
                    "transcribed: {} · failed/skipped: {}",
                    summary.transcribed, summary.transcribe_failed
                );
            }
        }
    }

    Ok(())
}
