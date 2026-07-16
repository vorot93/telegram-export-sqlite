mod render;
mod transcribe;

use crate::{
    error::{Result, TelegramExportError},
    export_rows::{load_export, validate_input_database},
    model::{ExportLlmOptions, ExportLlmSummary, OutputTarget},
};
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path};

pub fn run_export_llm(options: ExportLlmOptions) -> Result<ExportLlmSummary> {
    validate_options(&options)?;
    // Parse the command up front so a malformed template fails fast.
    let transcribe_command = options
        .transcribe
        .as_deref()
        .map(transcribe::TranscribeCommand::parse)
        .transpose()?;

    let conn = Connection::open_with_flags(
        &options.input_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    validate_input_database(&conn, &options.input_db)?;
    let export = load_export(&conn)?;

    let transcription = match &transcribe_command {
        Some(command) => {
            let db_dir = options
                .input_db
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            transcribe::transcribe_attachments(&export, db_dir, command)
        }
        None => transcribe::TranscriptionResult::default(),
    };

    let document = render::render_llm(&export, &transcription.transcripts);
    let stats = render::doc_stats(&export);

    match &options.output {
        OutputTarget::Stdout => write_stdout(&document)?,
        OutputTarget::File(path) => write_atomically(path, &document)?,
    }

    Ok(ExportLlmSummary {
        messages: export.messages.len(),
        service_events: export.service_events.len(),
        attachments: export.attachments.len(),
        polls: export.polls.len(),
        participants: stats.participants.len(),
        first_date: stats.first_date,
        last_date: stats.last_date,
        output_bytes: document.len(),
        estimated_tokens: render::estimate_tokens(&document),
        transcribed: transcription.transcribed,
        transcribe_failed: transcription.failed,
    })
}

fn validate_options(options: &ExportLlmOptions) -> Result<()> {
    if !options.input_db.is_file() {
        return Err(TelegramExportError::InputDatabaseMissing(
            options.input_db.clone(),
        ));
    }
    if let OutputTarget::File(path) = &options.output {
        if paths_equal(path, &options.input_db) {
            return Err(TelegramExportError::ExportOutputIsInputDatabase(
                path.clone(),
            ));
        }
        if path.exists() && !options.force {
            return Err(TelegramExportError::OutputFileExists(path.clone()));
        }
    }
    Ok(())
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Write the document to stdout, tolerating a downstream reader that closes the
/// pipe early (the headline `export-llm - | llm` workflow): a `BrokenPipe` error
/// is swallowed so the export exits cleanly instead of panicking, while any other
/// write error propagates.
pub(crate) fn write_stdout(document: &str) -> Result<()> {
    write_tolerating_broken_pipe(std::io::stdout().lock(), document.as_bytes())
}

/// Write `bytes` to `writer`, tolerating a reader that closes the pipe early (the
/// `export-llm - | head` workflow, and command summaries piped to `head`): a
/// `BrokenPipe` error resolves to `Ok(())` while any other error propagates.
pub(crate) fn write_tolerating_broken_pipe<W: std::io::Write>(
    mut writer: W,
    bytes: &[u8],
) -> Result<()> {
    match writer.write_all(bytes).and_then(|()| writer.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Write through a sibling temp file then atomically rename onto `path`.
fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    fs::write(&temp, contents)?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ErrWriter(std::io::ErrorKind);
    impl std::io::Write for ErrWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_is_tolerated_other_errors_propagate() {
        assert!(
            write_tolerating_broken_pipe(ErrWriter(std::io::ErrorKind::BrokenPipe), b"x").is_ok()
        );
        assert!(
            write_tolerating_broken_pipe(ErrWriter(std::io::ErrorKind::PermissionDenied), b"x")
                .is_err()
        );
    }
}
