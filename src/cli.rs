use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "telegram-export-sqlite")]
#[command(about = "Import Telegram Desktop exports to SQLite and export SQLite to HTML")]
#[command(
    after_help = "Usage:\n  telegram-export-sqlite import <EXPORT_DIR> [DEST]\n  telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>...\n  telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR>"
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
            println!("files seen: {}", summary.files_seen);
            println!("files imported: {}", summary.files_imported);
            println!("files skipped: {}", summary.files_skipped);
            println!("timeline items: {}", summary.timeline_items);
            println!("messages: {}", summary.messages);
            println!("service events: {}", summary.service_events);
            println!("attachments: {}", summary.attachments);
            println!("warnings: {}", summary.warnings);
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
            println!("input databases: {}", summary.input_databases);
            println!("timeline items: {}", summary.timeline_items);
            println!("messages: {}", summary.messages);
            println!("service events: {}", summary.service_events);
            println!("attachments: {}", summary.attachments);
            println!("duplicates skipped: {}", summary.duplicates_skipped);
            println!("conflicts kept: {}", summary.conflicts_kept);
            println!("warnings: {}", summary.warnings);
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
            println!("timeline items: {}", summary.timeline_items);
            println!("messages: {}", summary.messages);
            println!("service events: {}", summary.service_events);
            println!("attachments: {}", summary.attachments);
            println!("polls: {}", summary.polls);
            println!(
                "generated date separators: {}",
                summary.generated_date_separators
            );
        }
    }

    Ok(())
}
