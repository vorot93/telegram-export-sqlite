mod assets;
mod render;

use crate::{
    error::{Result, TelegramExportError},
    export_rows as rows,
    media_copy::{MediaCopyOutcome, copy_guarded},
    media_path::safe_media_path,
    model::{ExportHtmlOptions, ExportHtmlSummary},
    output_dir::{create_sibling_work_dir, replace_output_dir},
};
use rusqlite::{Connection, OpenFlags};
use std::{collections::HashSet, fs, path::Path};

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
    let write_result = (|| -> Result<usize> {
        assets::write_assets(&temp_dir)?;
        fs::write(temp_dir.join("messages.html"), html)?;
        let copied = copy_media_into_output(&options.input_db, &export.attachments, &temp_dir)?;
        replace_output_dir(&temp_dir, &options.output_dir)?;
        Ok(copied)
    })();
    let media_files_copied = match write_result {
        Ok(copied) => copied,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };

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
        media_files_copied,
    })
}

/// Copy each attachment's media into the export output so its `href`/`src`
/// resolves. Media is referenced by `relative_path` and resolved against the input
/// database's own directory (bundle DBs keep media under `assets/` next to
/// `chat.sqlite`; in-place DBs keep the original export layout), then copied to the
/// same relative path under `output_dir`, preserving the subtree so the paths the
/// renderer emitted point at the copied files. The shared [`copy_guarded`] applies
/// the anti-smuggling guards (a symlink/hard-link escaping the DB's directory is
/// skipped). A missing source is skipped silently — it was already recorded as a
/// warning at import time. Thumbnails are not copied because the renderer never
/// emits them. Returns the number of files copied.
fn copy_media_into_output(
    input_db: &Path,
    attachments: &[rows::AttachmentRow],
    output_dir: &Path,
) -> Result<usize> {
    let source_root = input_db.parent().unwrap_or_else(|| Path::new("."));
    // Canonicalize once so a source reachable only through a symlinked root still
    // matches; if the DB's own directory cannot be resolved there is nothing to copy.
    let Ok(canon_root) = fs::canonicalize(source_root) else {
        return Ok(0);
    };
    let mut seen = HashSet::new();
    let mut copied = 0usize;
    for attachment in attachments {
        let Some(raw) = attachment.relative_path.as_deref() else {
            continue;
        };
        // Only paths the renderer would actually link (same `safe_media_path` gate);
        // an unsafe path renders as a box with no href, so copying it would be dead weight.
        let Some(safe) = safe_media_path(raw) else {
            continue;
        };
        // The same media referenced by several attachments is copied once.
        if !seen.insert(safe.clone()) {
            continue;
        }
        let source = source_root.join(&safe);
        let target = output_dir.join(&safe);
        if copy_guarded(&canon_root, &source, &target)? == MediaCopyOutcome::Copied {
            copied += 1;
        }
    }
    Ok(copied)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn media_attachment(relative_path: &str) -> rows::AttachmentRow {
        rows::AttachmentRow {
            timeline_item_id: 1,
            attachment_kind: "file".to_string(),
            relative_path: Some(relative_path.to_string()),
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
            extra_json: "{}".to_string(),
        }
    }

    #[test]
    fn copy_media_into_output_copies_referenced_media_preserving_subtree() {
        let db_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(db_dir.path().join("chat_001/photos")).unwrap();
        std::fs::write(db_dir.path().join("chat_001/photos/p.jpg"), b"jpeg").unwrap();
        // The DB file itself need not exist; only its directory anchors media.
        let input_db = db_dir.path().join("chat.sqlite");
        let out = tempfile::tempdir().unwrap();

        let copied = copy_media_into_output(
            &input_db,
            &[media_attachment("chat_001/photos/p.jpg")],
            out.path(),
        )
        .unwrap();

        assert_eq!(copied, 1);
        assert_eq!(
            std::fs::read(out.path().join("chat_001/photos/p.jpg")).unwrap(),
            b"jpeg"
        );
    }

    #[test]
    fn copy_media_into_output_skips_missing_source() {
        let db_dir = tempfile::tempdir().unwrap();
        let input_db = db_dir.path().join("chat.sqlite");
        let out = tempfile::tempdir().unwrap();

        let copied =
            copy_media_into_output(&input_db, &[media_attachment("files/gone.pdf")], out.path())
                .unwrap();

        assert_eq!(copied, 0);
        assert!(!out.path().join("files/gone.pdf").exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_media_into_output_refuses_symlink_escaping_db_dir() {
        use std::os::unix::fs::symlink;

        let db_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.jpg");
        std::fs::write(&secret, b"top secret").unwrap();

        // A referenced media path that is a symlink pointing outside the DB's own
        // directory, the way a crafted DB could try to smuggle an arbitrary file
        // into the shareable HTML output.
        std::fs::create_dir_all(db_dir.path().join("photos")).unwrap();
        symlink(&secret, db_dir.path().join("photos/leak.jpg")).unwrap();
        let input_db = db_dir.path().join("chat.sqlite");
        let out = tempfile::tempdir().unwrap();

        let copied = copy_media_into_output(
            &input_db,
            &[media_attachment("photos/leak.jpg")],
            out.path(),
        )
        .unwrap();

        assert_eq!(copied, 0, "an escaping symlink must not be copied");
        assert!(
            !out.path().join("photos/leak.jpg").exists(),
            "the outside secret must not leak into the export output"
        );
    }
}
