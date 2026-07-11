mod assets;
mod render;
mod rows;

use crate::{
    error::{Result, TelegramExportError},
    model::{ExportHtmlOptions, ExportHtmlSummary},
    output_dir::{create_sibling_work_dir, replace_output_dir},
};
use rusqlite::{Connection, OpenFlags};
use std::fs;

pub fn run_export_html(options: ExportHtmlOptions) -> Result<ExportHtmlSummary> {
    validate_options(&options)?;
    let conn = Connection::open_with_flags(
        &options.input_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    rows::validate_input_database(&conn, &options.input_db)?;
    let export = rows::load_export(&conn)?;
    let rendered = render::render_history(&export)?;
    let html = render::render_page(&export.chat_title, &rendered.html);

    if let Some(parent) = options
        .output_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temp_dir = create_sibling_work_dir(&options.output_dir, "tmp")?;
    let write_result = (|| {
        assets::write_assets(&temp_dir)?;
        fs::write(temp_dir.join("messages.html"), html)?;
        replace_output_dir(&temp_dir, &options.output_dir)
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }

    Ok(ExportHtmlSummary {
        timeline_items: export
            .timeline_items
            .iter()
            .filter(|item| item.item_kind != "date_separator")
            .count(),
        messages: export.messages.len(),
        service_events: export.service_events.len(),
        attachments: export.attachments.len(),
        polls: export.polls.len(),
        generated_date_separators: rendered.generated_date_separators,
    })
}

fn validate_options(options: &ExportHtmlOptions) -> Result<()> {
    if !options.input_db.is_file() {
        return Err(TelegramExportError::InputDatabaseMissing(
            options.input_db.clone(),
        ));
    }
    if options.output_dir.is_file() {
        return Err(TelegramExportError::OutputPathIsFile(
            options.output_dir.clone(),
        ));
    }
    reject_input_inside_output(options)?;
    if options.output_dir.exists() && !options.force {
        return Err(TelegramExportError::OutputDirectoryExists(
            options.output_dir.clone(),
        ));
    }
    Ok(())
}

fn reject_input_inside_output(options: &ExportHtmlOptions) -> Result<()> {
    if !options.output_dir.exists() {
        return Ok(());
    }

    let input = fs::canonicalize(&options.input_db)?;
    let output = fs::canonicalize(&options.output_dir)?;
    if input.starts_with(&output) {
        return Err(TelegramExportError::ExportInputInsideOutput {
            input: options.input_db.clone(),
            output: options.output_dir.clone(),
        });
    }

    Ok(())
}
