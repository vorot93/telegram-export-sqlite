use crate::{
    db,
    db_swap::{cleanup_temp_database, replace_database, temporary_database_path},
    discovery::{discover_json_export_file, discover_messages_files},
    error::{Result, TelegramExportError},
    json_parser::parse_json_export_file,
    model::{ImportOptions, ImportSummary, ParsedExport, SourceFile},
    output_dir::{create_sibling_work_dir, replace_output_dir},
    parser::{ParserState, parse_export_file_with_state},
};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

pub const DATABASE_FILE_NAME: &str = "chat.sqlite";

pub fn run_import(options: ImportOptions) -> Result<ImportSummary> {
    match options.dest.clone() {
        None => run_in_place_import(&options),
        Some(dest) => run_bundle_import(&options, &dest),
    }
}

fn run_in_place_import(options: &ImportOptions) -> Result<ImportSummary> {
    let output_db = options.input_dir.join(DATABASE_FILE_NAME);
    if options.incremental {
        if !output_db.exists() {
            return Err(TelegramExportError::IncrementalDatabaseMissing(output_db));
        }
    } else if output_db.exists() && !options.force {
        return Err(TelegramExportError::OutputDatabaseExists(output_db));
    }
    build_database(options, &output_db, &output_db)
}

/// Build a portable bundle at `dest`: `{DEST}/chat.sqlite` plus `{DEST}/assets/`
/// with relocated media. The database is built in a sibling temp directory and
/// `dest` is replaced atomically only once the build succeeds.
fn run_bundle_import(options: &ImportOptions, dest: &Path) -> Result<ImportSummary> {
    if dest.is_file() {
        return Err(TelegramExportError::OutputPathIsFile(dest.to_path_buf()));
    }
    reject_dest_overlapping_export(&options.input_dir, dest)?;

    let existing_db = dest.join(DATABASE_FILE_NAME);
    // Recover or announce a bundle stranded in a `.<name>.backup-*` sibling by a
    // crash in a prior replace_output_dir. For an incremental refresh whose DEST
    // vanished in that crash, this restores the lone backup so the retry proceeds
    // instead of hard-failing below.
    announce_or_recover_stray_backups(dest, options.incremental)?;
    if options.incremental {
        if !existing_db.exists() {
            return Err(TelegramExportError::IncrementalDatabaseMissing(existing_db));
        }
    } else if dest.exists() && !options.force {
        return Err(TelegramExportError::OutputDirectoryExists(
            dest.to_path_buf(),
        ));
    }

    if let Some(parent) = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let temp_bundle = create_sibling_work_dir(dest, "tmp")?;
    let result = (|| {
        let temp_db = temp_bundle.join(DATABASE_FILE_NAME);
        let mut summary = build_database(options, &temp_db, &existing_db)?;
        let report = crate::bundle::relocate_media(&temp_db, &options.input_dir, &temp_bundle)?;
        summary.warnings += report.warnings();
        // An incremental refresh rebuilds the bundle from scratch and then
        // atomically replaces DEST, so any media the new export no longer
        // provides would be permanently deleted. Preserve it: union the prior
        // DEST/assets into the freshly built bundle for every path the new bundle
        // does not already contain (freshly copied media wins on a path clash).
        if options.incremental {
            preserve_prior_assets(
                &dest.join(crate::bundle::ASSETS_DIR),
                &temp_bundle.join(crate::bundle::ASSETS_DIR),
            )?;
        }
        replace_output_dir(&temp_bundle, dest)?;
        Ok(summary)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp_bundle);
    }
    result
}

/// Surface `.<name>.backup-*` directories a crash in a prior bundle replacement
/// left beside DEST. For an incremental refresh whose DEST is now missing (the
/// exact post-crash retry state), restore a single unambiguous backup so the run
/// can proceed; otherwise announce the strays on stderr and leave them in place
/// (ambiguous or DEST-present cases are the user's to resolve).
fn announce_or_recover_stray_backups(dest: &Path, incremental: bool) -> Result<()> {
    let backups = crate::output_dir::find_stray_backup_dirs(dest)?;
    if backups.is_empty() {
        return Ok(());
    }

    if incremental && !dest.exists() {
        let recoverable: Vec<&PathBuf> = backups
            .iter()
            .filter(|backup| backup.join(DATABASE_FILE_NAME).is_file())
            .collect();
        if let [backup] = recoverable.as_slice() {
            fs::rename(backup, dest)?;
            eprintln!(
                "recovered a stray backup from a previous interrupted run: {} -> {}",
                backup.display(),
                dest.display()
            );
            return Ok(());
        }
    }

    for backup in &backups {
        eprintln!(
            "warning: a previous run was interrupted; a stray backup was left in place: {}",
            backup.display()
        );
    }
    Ok(())
}

/// Copy every file under `prior_assets` into `new_assets` at the same relative
/// path unless the new bundle already wrote that path (freshly copied media wins).
/// This makes an incremental bundle refresh additive for media: a file the new
/// export dropped stays archived rather than being deleted by the atomic bundle
/// replacement. No-op when the prior bundle had no `assets/`.
fn preserve_prior_assets(prior_assets: &Path, new_assets: &Path) -> Result<()> {
    if !prior_assets.is_dir() {
        return Ok(());
    }
    copy_missing_files(prior_assets, new_assets)
}

fn copy_missing_files(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src = entry.path();
        let dst = dst_dir.join(entry.file_name());
        if file_type.is_dir() {
            copy_missing_files(&src, &dst)?;
        } else if file_type.is_file() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dst)?;
        }
        // bundle.rs only ever writes regular files/dirs under assets/, so anything
        // else here (a symlink a user planted) is ignored rather than followed.
    }
    Ok(())
}

fn reject_dest_overlapping_export(input_dir: &Path, dest: &Path) -> Result<()> {
    let export = lexical_abs(input_dir);
    let target = lexical_abs(dest);
    if target == export || target.starts_with(&export) || export.starts_with(&target) {
        return Err(TelegramExportError::BundleDestOverlapsExport {
            dest: dest.to_path_buf(),
            export: input_dir.to_path_buf(),
        });
    }
    Ok(())
}

/// Absolute, lexically-normalized path (no filesystem access, so it works for a
/// destination that does not exist yet). Resolves `.`/`..` textually.
fn lexical_abs(path: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Build the SQLite database at `output_db`. For incremental imports,
/// `existing_db` is the database consulted for already-finished source files
/// (equal to `output_db` for in-place imports; the destination bundle's DB for
/// bundle imports).
fn build_database(
    options: &ImportOptions,
    output_db: &Path,
    existing_db: &Path,
) -> Result<ImportSummary> {
    let sources = discover_export_sources(&options.input_dir)?;

    if options.incremental {
        let mut conn = Connection::open(existing_db)?;
        // Gate on the schema version BEFORE create_schema stamps user_version, so
        // an incremental run never mutates a database it then rejects (a file at
        // any other schema version, or a foreign SQLite database sitting at the
        // destination).
        // Merge and export gate the same way via validate_input_database.
        crate::export_rows::validate_input_database(&conn, existing_db)?;
        db::create_schema(&conn)?;
        let existing_fts = has_fts_table(&conn)?;
        ensure_finished_source_paths_present(&conn, sources.files())?;
        if output_db == existing_db && all_source_files_are_finished(&conn, sources.files())? {
            return finish_all_skipped_incremental_import(
                &mut conn,
                options,
                existing_db,
                sources.files().len(),
            );
        }
        drop(conn);
        return run_full_rebuild_safely(
            options,
            &sources,
            output_db,
            existing_db,
            "incremental",
            options.fts || existing_fts,
        );
    }

    let create_fts = options.fts || existing_database_has_fts_if_readable(existing_db)?;
    run_full_rebuild_safely(
        options,
        &sources,
        output_db,
        existing_db,
        "rebuild",
        create_fts,
    )
}

enum ExportSources {
    Html(Vec<SourceFile>),
    Json(Vec<SourceFile>),
}

impl ExportSources {
    fn files(&self) -> &[SourceFile] {
        match self {
            Self::Html(source_files) | Self::Json(source_files) => source_files,
        }
    }
}

fn discover_export_sources(input_dir: &Path) -> Result<ExportSources> {
    if let Some(source_file) = discover_json_export_file(input_dir)? {
        return Ok(ExportSources::Json(vec![source_file]));
    }

    discover_messages_files(input_dir).map(ExportSources::Html)
}

fn finish_all_skipped_incremental_import(
    conn: &mut Connection,
    options: &ImportOptions,
    recorded_output_path: &Path,
    files_seen: usize,
) -> Result<ImportSummary> {
    let tx = conn.transaction()?;
    let import_id = db::begin_import(&tx, &options.input_dir, recorded_output_path, "incremental")?;
    let summary = ImportSummary {
        files_seen,
        files_skipped: files_seen,
        ..Default::default()
    };

    if options.fts {
        db::create_fts(&tx)?;
    }
    db::finish_import(&tx, import_id, &summary)?;
    tx.commit()?;

    Ok(summary)
}

fn run_full_rebuild_safely(
    options: &ImportOptions,
    sources: &ExportSources,
    output_db: &Path,
    recorded_output_path: &Path,
    mode: &str,
    create_fts: bool,
) -> Result<ImportSummary> {
    let temp_path = temporary_database_path(output_db);
    let import_result = import_all_files_to_database(
        options,
        sources,
        &temp_path,
        recorded_output_path,
        mode,
        create_fts,
    )
    .and_then(|summary| {
        // For an incremental refresh, refuse before the destructive replace if the
        // freshly built database is a different chat than the one archived, so
        // pointing --incremental at an unrelated export can't silently overwrite
        // the archive. `recorded_output_path` still holds the prior database here
        // (in-place: the output DB not yet replaced; bundle: DEST/chat.sqlite).
        if options.incremental {
            refuse_incremental_chat_mismatch(recorded_output_path, &temp_path)?;
        }
        replace_database(&temp_path, output_db)?;
        cleanup_temp_database(&temp_path);
        Ok(summary)
    });

    if import_result.is_err() {
        cleanup_temp_database(&temp_path);
    }

    import_result
}

/// Refuse an incremental refresh that targets a different chat than the one
/// already archived. Incremental keys only on `(relative_path, checksum)`, so two
/// unrelated chats sharing an export layout (both `result.json`, or both
/// `chat_001/messages.html`) would otherwise let a rebuild silently replace the
/// archive. Chat identity is the set of chat titles (current title plus every
/// past alias); a rename whose new title was never seen before is a documented
/// false positive, recoverable with a fresh import.
fn refuse_incremental_chat_mismatch(existing_db: &Path, new_db: &Path) -> Result<()> {
    let existing = chat_titles(existing_db)?;
    let incoming = chat_titles(new_db)?;
    if !existing.is_empty() && !incoming.is_empty() && existing.is_disjoint(&incoming) {
        return Err(TelegramExportError::IncrementalChatMismatch {
            existing: existing.into_iter().next().unwrap_or_default(),
            incoming: incoming.into_iter().next().unwrap_or_default(),
        });
    }
    Ok(())
}

/// The set of chat titles recorded in a database: `chats.title` plus every
/// `chat_aliases.title` ever seen, so a chat that has cycled between known names
/// still matches itself. An unreadable/absent database yields an empty set, which
/// disables the mismatch check rather than misfiring on it.
fn chat_titles(db_path: &Path) -> Result<BTreeSet<String>> {
    let conn = Connection::open(db_path)?;
    let mut titles = BTreeSet::new();
    for query in ["SELECT title FROM chats", "SELECT title FROM chat_aliases"] {
        let mut stmt = conn.prepare(query)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for title in rows {
            titles.insert(title?);
        }
    }
    Ok(titles)
}

fn import_all_files_to_database(
    options: &ImportOptions,
    sources: &ExportSources,
    database_path: &Path,
    recorded_output_path: &Path,
    mode: &str,
    create_fts: bool,
) -> Result<ImportSummary> {
    let mut conn = Connection::open(database_path)?;
    db::create_schema(&conn)?;

    let tx = conn.transaction()?;
    let import_id = db::begin_import(&tx, &options.input_dir, recorded_output_path, mode)?;
    let mut summary = ImportSummary {
        files_seen: sources.files().len(),
        ..Default::default()
    };

    match sources {
        ExportSources::Html(source_files) => {
            let mut parser_state = ParserState::default();
            let mut next_ordinal = 0;

            for source_file in source_files {
                let parsed = parse_export_file_with_state(
                    &options.input_dir,
                    &source_file.absolute_path,
                    &source_file.relative_path,
                    source_file.parse_order,
                    next_ordinal,
                    &mut parser_state,
                )?;
                next_ordinal += i64::try_from(parsed.timeline_items.len()).map_err(|_| {
                    TelegramExportError::Parse(
                        "timeline item count does not fit in i64".to_string(),
                    )
                })?;
                // Store the checksum of the bytes the parser actually read, not
                // discovery's: a concurrent rewrite between the two reads would
                // otherwise leave a stored checksum that disagrees with the
                // imported content and mislead incremental skips. See C39.
                let stored = SourceFile {
                    checksum: parsed.source_checksum.clone(),
                    ..source_file.clone()
                };
                let chat_title = parsed.chat.as_ref().map(|chat| chat.title.as_str());
                db::insert_source_file(&tx, import_id, &stored, chat_title)?;
                db::insert_parsed_export(&tx, import_id, std::slice::from_ref(&stored), &parsed)?;
                summary.files_imported += 1;
                add_parsed_counts(&mut summary, &parsed);
            }
        }
        ExportSources::Json(source_files) => {
            let source_file = source_files.first().ok_or_else(|| {
                TelegramExportError::Parse("JSON source list is empty".to_string())
            })?;
            let parsed = parse_json_export_file(
                &options.input_dir,
                &source_file.absolute_path,
                &source_file.relative_path,
                source_file.parse_order,
                0,
            )?;
            // Store the parse-time checksum rather than discovery's (see the HTML
            // branch and C39): it always matches the imported content.
            let stored = SourceFile {
                checksum: parsed.source_checksum.clone(),
                ..source_file.clone()
            };
            let chat_title = parsed.chat.as_ref().map(|chat| chat.title.as_str());
            db::insert_source_file(&tx, import_id, &stored, chat_title)?;
            db::insert_parsed_export(&tx, import_id, std::slice::from_ref(&stored), &parsed)?;
            summary.files_imported += 1;
            add_parsed_counts(&mut summary, &parsed);
        }
    }

    if create_fts {
        db::create_fts(&tx)?;
    }
    db::finish_import(&tx, import_id, &summary)?;
    tx.commit()?;

    Ok(summary)
}

fn all_source_files_are_finished(conn: &Connection, source_files: &[SourceFile]) -> Result<bool> {
    for source_file in source_files {
        if !has_finished_source_file_identity(conn, source_file)? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn has_finished_source_file_identity(conn: &Connection, source_file: &SourceFile) -> Result<bool> {
    let relative_path = source_file.relative_path.to_string_lossy();
    let exists = conn.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM source_files sf
            JOIN imports i ON i.id = sf.import_id
            WHERE i.status = 'finished'
              AND sf.relative_path = ?1
              AND sf.checksum = ?2
        )",
        params![relative_path.as_ref(), source_file.checksum],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

fn ensure_finished_source_paths_present(
    conn: &Connection,
    source_files: &[SourceFile],
) -> Result<()> {
    let current_paths: BTreeSet<String> = source_files
        .iter()
        .map(|source_file| source_file.relative_path.to_string_lossy().into_owned())
        .collect();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sf.relative_path
         FROM source_files sf
         JOIN imports i ON i.id = sf.import_id
         WHERE i.status = 'finished'
         ORDER BY sf.relative_path",
    )?;
    let previous_paths = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut missing_paths = Vec::new();

    for previous_path in previous_paths {
        let previous_path = previous_path?;
        if !current_paths.contains(&previous_path) {
            missing_paths.push(previous_path);
        }
    }

    if !missing_paths.is_empty() {
        return Err(TelegramExportError::Parse(format!(
            "incremental rebuild refused because previously imported source files are missing from the current export: {}",
            missing_paths.join(", ")
        )));
    }

    Ok(())
}

fn has_fts_table(conn: &Connection) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM sqlite_master
            WHERE type = 'table'
              AND name = 'timeline_items_fts'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

fn existing_database_has_fts_if_readable(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let Ok(conn) = Connection::open(path) else {
        return Ok(false);
    };
    match has_fts_table(&conn) {
        Ok(exists) => Ok(exists),
        Err(TelegramExportError::Sqlite(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

fn add_parsed_counts(summary: &mut ImportSummary, parsed: &ParsedExport) {
    summary.timeline_items += parsed.timeline_items.len();
    summary.messages += parsed.messages.len();
    summary.service_events += parsed.service_events.len();
    summary.attachments += parsed.attachments.len();
    summary.warnings += parsed.warnings.len();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;
    use crate::error::TelegramExportError;

    pub(crate) fn import_options(
        input_dir: &Path,
        force: bool,
        incremental: bool,
        fts: bool,
    ) -> ImportOptions {
        ImportOptions {
            input_dir: input_dir.to_path_buf(),
            dest: None,
            force,
            incremental,
            fts,
        }
    }

    /// Recursively copy a directory tree (std-only; no extra deps).
    pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let target = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// Copy a fixture export into a fresh writable tempdir.
    pub(crate) fn staged_export(fixture: &Path) -> tempfile::TempDir {
        let temp = tempdir().unwrap();
        copy_dir_recursive(fixture, temp.path());
        temp
    }

    #[test]
    fn stored_source_checksum_hashes_the_parsed_content() {
        // The checksum written for each source file must be the hash of the exact
        // bytes the parser read, so it can never disagree with the imported
        // content (which would let incremental skip a file it should re-import).
        let staged = staged_export(&fixture_dir());
        run_import(import_options(staged.path(), true, false, false)).unwrap();

        let conn = Connection::open(staged.path().join(DATABASE_FILE_NAME)).unwrap();
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT relative_path, checksum FROM source_files")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            !rows.is_empty(),
            "the fixture imports at least one source file"
        );
        for (relative_path, checksum) in rows {
            let bytes = fs::read(staged.path().join(&relative_path)).unwrap();
            assert_eq!(
                checksum,
                crate::discovery::sha256_hex(&bytes),
                "stored checksum for {relative_path} must hash its content"
            );
        }
    }

    #[test]
    fn refuses_existing_database_without_force_or_incremental() {
        let staged = staged_export(&fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);
        fs::write(&output_db, "already here").unwrap();

        let error = run_import(import_options(staged.path(), false, false, false))
            .expect_err("existing database should be refused");

        assert!(matches!(
            error,
            TelegramExportError::OutputDatabaseExists(path) if path == output_db
        ));
    }

    #[test]
    fn refuses_incremental_when_database_missing() {
        let staged = tempdir().unwrap(); // empty export dir, no chat.sqlite
        fs::create_dir_all(staged.path().join("empty")).unwrap();
        let input = staged.path().join("empty");

        let error = run_import(import_options(&input, false, true, false))
            .expect_err("incremental import should require an existing database");

        assert!(matches!(
            error,
            TelegramExportError::IncrementalDatabaseMissing(path) if path == input.join(DATABASE_FILE_NAME)
        ));
    }

    #[test]
    fn imports_fixture_with_force() {
        let staged = staged_export(&fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);
        fs::write(&output_db, "stale database").unwrap();

        let summary = run_import(import_options(staged.path(), true, false, true)).unwrap();

        assert_eq!(summary.files_seen, 2);
        assert_eq!(summary.files_imported, 2);
        assert_eq!(summary.attachments, 2);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            query_count(&conn, "SELECT COUNT(*) FROM timeline_items"),
            summary.timeline_items
        );
    }

    #[test]
    fn imports_json_export_when_result_json_is_present() {
        let staged = staged_export(&json_fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);

        let summary = run_import(import_options(staged.path(), true, false, false)).unwrap();

        assert_eq!(summary.files_seen, 1);
        assert_eq!(summary.files_imported, 1);
        assert_eq!(summary.files_skipped, 0);
        assert_eq!(summary.timeline_items, 4);
        assert_eq!(summary.messages, 3);
        assert_eq!(summary.service_events, 1);
        assert_eq!(summary.attachments, 1);
        assert_eq!(summary.warnings, 0);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT relative_path FROM source_files ORDER BY parse_order",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "result.json"
        );
        assert_eq!(
            conn.query_row("SELECT title FROM chats", [], |row| row.get::<_, String>(0),)
                .unwrap(),
            "Family Chat"
        );
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM polls"), 1);
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*)
                 FROM messages
                 JOIN timeline_items ON timeline_items.id = messages.timeline_item_id
                 WHERE timeline_items.display_text = 'Hello family link'"
            ),
            1
        );
    }

    #[test]
    fn incremental_json_import_skips_unchanged_result_file() {
        let staged = staged_export(&json_fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);

        run_import(import_options(staged.path(), true, false, false)).unwrap();
        let incremental = run_import(import_options(staged.path(), false, true, false)).unwrap();

        assert_eq!(incremental.files_seen, 1);
        assert_eq!(incremental.files_imported, 0);
        assert_eq!(incremental.files_skipped, 1);
        assert_eq!(incremental.timeline_items, 0);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 1);
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM timeline_items"), 4);
    }

    #[test]
    fn incremental_refuses_foreign_schema_version_without_mutating_it() {
        // A pre-existing database whose user_version is not the current
        // SCHEMA_VERSION (a newer tool wrote it, or a foreign SQLite file sits at
        // the destination) must be refused by an incremental import BEFORE the
        // schema is (re)stamped, so the run never mutates a database it then
        // rejects. Merge and export already gate on the version; import did not.
        let staged = staged_export(&json_fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);

        run_import(import_options(staged.path(), true, false, false)).unwrap();
        {
            let conn = Connection::open(&output_db).unwrap();
            conn.pragma_update(None, "user_version", 999).unwrap();
        }

        let error = run_import(import_options(staged.path(), false, true, false))
            .expect_err("incremental import must refuse a mismatched schema version");
        assert!(matches!(
            error,
            TelegramExportError::UnsupportedSchemaVersion { version, .. } if version == 999
        ));

        let version: i64 = Connection::open(&output_db)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 999,
            "the refused database's user_version must be left untouched"
        );
    }

    #[test]
    fn incremental_refuses_a_different_chat_without_replacing_the_archive() {
        // Incremental keys on (relative_path, checksum) only; two unrelated chats
        // both exported as result.json share that path, so a naive rebuild would
        // silently replace the archived chat with the different one. Refuse
        // instead, leaving the archive intact.
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        fs::create_dir_all(&export_dir).unwrap();

        let write_chat = |name: &str, text: &str| {
            fs::write(
                export_dir.join("result.json"),
                format!(
                    r#"{{"name":"{name}","type":"personal_chat","id":1,"messages":[
                        {{"id":1,"type":"message","date":"2025-01-01T00:00:00","date_unixtime":"1735689600","from":"Me","from_id":"user1","text":"{text}"}}
                    ]}}"#
                ),
            )
            .unwrap();
        };

        write_chat("Chat A", "hello from A");
        run_import(import_options(&export_dir, true, false, false)).unwrap();

        write_chat("Chat B", "hello from B");
        let error = run_import(import_options(&export_dir, false, true, false))
            .expect_err("incremental must refuse a different chat");
        assert!(matches!(
            error,
            TelegramExportError::IncrementalChatMismatch { .. }
        ));

        // The original archive is untouched: still Chat A, still its one message.
        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            conn.query_row("SELECT title FROM chats", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Chat A"
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'hello from A'"
            ),
            1
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'hello from B'"
            ),
            0
        );
    }

    #[test]
    fn malformed_json_import_preserves_existing_database() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("json_export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_minimal_json_export(&export_dir, "Original text");

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        fs::write(export_dir.join("result.json"), "{ malformed json").unwrap();

        let error = run_import(import_options(&export_dir, true, false, false))
            .expect_err("malformed JSON should fail without replacing the existing DB");

        assert!(matches!(error, TelegramExportError::Json(_)));
        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'Original text'",
            ),
            1
        );
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 1);
    }

    #[test]
    fn import_refuses_multi_chat_export_and_preserves_existing_database() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("account_export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        fs::create_dir_all(&export_dir).unwrap();

        // Seed an existing single-chat archive, then prove a refused multi-chat
        // import leaves it intact.
        fs::write(
            export_dir.join("result.json"),
            r#"{"name":"Solo","type":"personal_chat","id":1,"messages":[
                {"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Me","from_id":"user1","text":"kept"}
            ]}"#,
        )
        .unwrap();
        run_import(import_options(&export_dir, true, false, false)).unwrap();

        // Replace it with a full-account (multi-chat) export at the same path.
        fs::write(
            export_dir.join("result.json"),
            r#"{"chats":{"list":[
                {"name":"Alice","type":"personal_chat","id":111,"messages":[{"id":1,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Alice","from_id":"user111","text":"a"}]},
                {"name":"Group","type":"private_group","id":222,"messages":[{"id":2,"type":"message","date":"2020-01-01T00:00:00","date_unixtime":"1577836800","from":"Bob","from_id":"user222","text":"b"}]}
            ]}}"#,
        )
        .unwrap();

        let error = run_import(import_options(&export_dir, true, false, false))
            .expect_err("multi-chat import must be refused");
        assert!(matches!(
            error,
            TelegramExportError::MultiChatExportNotSupported { chats } if chats == 2
        ));

        // The prior single-chat archive is untouched.
        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            conn.query_row("SELECT title FROM chats", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Solo"
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'kept'"
            ),
            1
        );
    }

    #[test]
    fn uses_stateful_parser_across_files() {
        let temp = tempdir().unwrap();
        let chat_dir = temp.path().join("chat_001");
        fs::create_dir_all(&chat_dir).unwrap();
        fs::write(
            chat_dir.join("messages.html"),
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix" id="message601">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                <div class="from_name">Alice</div>
                <div class="text">First file</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        )
        .unwrap();
        fs::write(
            chat_dir.join("messages2.html"),
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history">
              <div class="message default clearfix joined" id="message602">
               <div class="body">
                <div class="pull_right date details" title="12.02.2025 08:38:01 UTC">08:38</div>
                <div class="text">Second file joined line</div>
               </div>
              </div>
             </div>
            </body>
            </html>
            "#,
        )
        .unwrap();
        let output_db = temp.path().join(DATABASE_FILE_NAME);

        let summary = run_import(import_options(temp.path(), true, false, false)).unwrap();

        assert_eq!(summary.files_seen, 2);
        assert_eq!(summary.files_imported, 2);
        assert_eq!(summary.messages, 2);

        let conn = Connection::open(&output_db).unwrap();
        let (sender, inferred, ordinal) = conn
            .query_row(
                "SELECT users.display_name, messages.sender_inferred, timeline_items.ordinal
                 FROM messages
                 JOIN timeline_items ON timeline_items.id = messages.timeline_item_id
                 LEFT JOIN users ON users.id = messages.sender_user_id
                 WHERE messages.telegram_message_id = 602",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(sender.as_deref(), Some("Alice"));
        assert_eq!(inferred, 1);
        assert_eq!(ordinal, 1);
    }

    #[test]
    fn imports_split_files_in_telegram_natural_order() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "First page",
        );
        write_message_file(
            &export_dir,
            "messages10.html",
            10,
            Some("Carol"),
            false,
            "Tenth page",
        );
        write_message_file(
            &export_dir,
            "messages2.html",
            2,
            Some("Bob"),
            false,
            "Second page",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();

        let conn = Connection::open(&output_db).unwrap();
        let rows: Vec<(i64, i64)> = conn
            .prepare(
                "SELECT ordinal, telegram_message_id
                 FROM timeline_items
                 WHERE item_kind = 'message'
                 ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(rows, vec![(0, 1), (1, 2), (2, 10)]);
    }

    #[test]
    fn incremental_skips_finished_imported_files() {
        let staged = staged_export(&fixture_dir());
        let output_db = staged.path().join(DATABASE_FILE_NAME);

        let initial = run_import(import_options(staged.path(), true, false, false)).unwrap();
        let conn = Connection::open(&output_db).unwrap();
        let initial_timeline_rows = query_count(&conn, "SELECT COUNT(*) FROM timeline_items");
        drop(conn);

        let incremental = run_import(import_options(staged.path(), false, true, false)).unwrap();

        assert_eq!(initial.files_imported, 2);
        assert_eq!(incremental.files_seen, 2);
        assert_eq!(incremental.files_imported, 0);
        assert_eq!(incremental.files_skipped, 2);
        assert_eq!(incremental.timeline_items, 0);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            query_count(&conn, "SELECT COUNT(*) FROM timeline_items"),
            initial_timeline_rows
        );
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM imports"), 2);
    }

    #[test]
    fn force_import_failure_preserves_existing_database() {
        let temp = tempdir().unwrap();
        let input_dir = temp.path().join("empty_export");
        let output_db = input_dir.join(DATABASE_FILE_NAME);
        fs::create_dir_all(&input_dir).unwrap();
        fs::write(&output_db, "existing database bytes").unwrap();

        let error = run_import(import_options(&input_dir, true, false, false))
            .expect_err("empty export should fail before replacing the existing database");

        assert!(matches!(error, TelegramExportError::NoMessagesFiles(path) if path == input_dir));
        assert_eq!(
            fs::read_to_string(&output_db).unwrap(),
            "existing database bytes"
        );
    }

    #[test]
    fn incremental_modified_file_rebuilds_without_duplicate_timeline_rows() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "First text",
        );
        write_message_file(
            &export_dir,
            "messages2.html",
            2,
            Some("Bob"),
            false,
            "Second text",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        let conn = Connection::open(&output_db).unwrap();
        let initial_timeline_rows = query_count(&conn, "SELECT COUNT(*) FROM timeline_items");
        drop(conn);

        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "Changed first text",
        );
        run_import(import_options(&export_dir, false, true, false)).unwrap();

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            query_count(&conn, "SELECT COUNT(*) FROM timeline_items"),
            initial_timeline_rows
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM (
                    SELECT telegram_message_id
                    FROM timeline_items
                    WHERE telegram_message_id IS NOT NULL
                    GROUP BY telegram_message_id
                    HAVING COUNT(*) > 1
                )",
            ),
            0
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'Changed first text'",
            ),
            1
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'First text'",
            ),
            0
        );
    }

    #[test]
    fn incremental_rebuild_refuses_missing_previously_imported_source_file() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        let chat_dir = export_dir.join("chat_001");
        write_message_file(
            &export_dir,
            "messages.html",
            801,
            Some("Alice"),
            false,
            "Original first",
        );
        write_message_file(
            &export_dir,
            "messages2.html",
            802,
            Some("Bob"),
            false,
            "Original second",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        fs::remove_file(chat_dir.join("messages2.html")).unwrap();
        write_message_file(
            &export_dir,
            "messages.html",
            801,
            Some("Alice"),
            false,
            "Changed first",
        );

        let error = run_import(import_options(&export_dir, false, true, false))
            .expect_err("incremental rebuild should refuse incomplete source file set");

        assert!(matches!(
            error,
            TelegramExportError::Parse(message)
                if message.contains("incremental rebuild refused")
                    && message.contains("chat_001/messages2.html")
        ));

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 2);
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM timeline_items"), 2);
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'Original first'",
            ),
            1
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'Original second'",
            ),
            1
        );
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items WHERE display_text = 'Changed first'",
            ),
            0
        );
    }

    #[test]
    fn incremental_skip_refuses_missing_previously_imported_source_file() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "Original first",
        );
        write_message_file(
            &export_dir,
            "messages2.html",
            2,
            Some("Bob"),
            false,
            "Original second",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        fs::remove_file(export_dir.join("chat_001/messages2.html")).unwrap();

        let error = run_import(import_options(&export_dir, false, true, false))
            .expect_err("incremental skip should refuse incomplete source file set");

        assert!(matches!(
            error,
            TelegramExportError::Parse(message)
                if message.contains("incremental rebuild refused")
                    && message.contains("chat_001/messages2.html")
        ));

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 2);
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM timeline_items"), 2);
    }

    #[test]
    fn incremental_new_joined_file_preserves_sender_context() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_message_file(
            &export_dir,
            "messages.html",
            601,
            Some("Alice"),
            false,
            "First file",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        write_message_file(
            &export_dir,
            "messages2.html",
            602,
            None,
            true,
            "Second file joined line",
        );
        run_import(import_options(&export_dir, false, true, false)).unwrap();

        let conn = Connection::open(&output_db).unwrap();
        let (sender, inferred) = conn
            .query_row(
                "SELECT users.display_name, messages.sender_inferred
                 FROM messages
                 LEFT JOIN users ON users.id = messages.sender_user_id
                 WHERE messages.telegram_message_id = 602",
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();

        assert_eq!(sender.as_deref(), Some("Alice"));
        assert_eq!(inferred, 1);
    }

    #[test]
    fn incremental_identical_new_file_rebuilds_by_source_identity() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        let chat_dir = export_dir.join("chat_001");
        write_message_file(
            &export_dir,
            "messages.html",
            701,
            Some("Alice"),
            false,
            "Repeated export page",
        );

        run_import(import_options(&export_dir, true, false, false)).unwrap();
        fs::copy(
            chat_dir.join("messages.html"),
            chat_dir.join("messages2.html"),
        )
        .unwrap();

        let incremental = run_import(import_options(&export_dir, false, true, false)).unwrap();

        assert_eq!(incremental.files_seen, 2);
        assert_eq!(incremental.files_imported, 2);
        assert_eq!(incremental.files_skipped, 0);
        assert_eq!(incremental.timeline_items, 2);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 2);
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM timeline_items"), 2);
    }

    #[test]
    fn empty_messages_file_is_recorded_and_skipped_later() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_empty_messages_file(&export_dir, "messages.html");

        let initial = run_import(import_options(&export_dir, true, false, false)).unwrap();

        assert_eq!(initial.files_seen, 1);
        assert_eq!(initial.files_imported, 1);
        assert_eq!(initial.timeline_items, 0);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 1);
        drop(conn);

        let incremental = run_import(import_options(&export_dir, false, true, false)).unwrap();

        assert_eq!(incremental.files_seen, 1);
        assert_eq!(incremental.files_imported, 0);
        assert_eq!(incremental.files_skipped, 1);

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(query_count(&conn, "SELECT COUNT(*) FROM source_files"), 1);
    }

    #[test]
    fn existing_fts_is_refreshed_after_incremental_rebuild_without_fts_flag() {
        let temp = tempdir().unwrap();
        let export_dir = temp.path().join("export");
        let output_db = export_dir.join(DATABASE_FILE_NAME);
        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "Original term",
        );

        run_import(import_options(&export_dir, true, false, true)).unwrap();
        write_message_file(
            &export_dir,
            "messages.html",
            1,
            Some("Alice"),
            false,
            "needleword",
        );
        run_import(import_options(&export_dir, false, true, false)).unwrap();

        let conn = Connection::open(&output_db).unwrap();
        assert_eq!(
            query_count(
                &conn,
                "SELECT COUNT(*) FROM timeline_items_fts WHERE timeline_items_fts MATCH 'needleword'",
            ),
            1
        );
    }

    #[test]
    fn bundle_import_records_final_dest_path_as_output_path() {
        let staged = staged_export(&fixture_dir());
        let dir = tempdir().unwrap();
        let dest = dir.path().join("archive");

        run_import(ImportOptions {
            input_dir: staged.path().to_path_buf(),
            dest: Some(dest.clone()),
            force: false,
            incremental: false,
            fts: false,
        })
        .unwrap();

        let conn = Connection::open(dest.join(DATABASE_FILE_NAME)).unwrap();
        let recorded_output_path: String = conn
            .query_row(
                "SELECT output_path FROM imports ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // Must be the final bundle path, not the sibling temp directory the
        // database was actually built in before being renamed into place.
        assert_eq!(
            recorded_output_path,
            dest.join(DATABASE_FILE_NAME).to_string_lossy().into_owned()
        );
    }

    #[test]
    fn incremental_bundle_refresh_preserves_previously_archived_media() {
        // An incremental bundle refresh rebuilds into a temp bundle and then
        // atomically replaces DEST. Media the new export no longer provides (here
        // a deleted file still referenced by the unchanged messages.html) must not
        // be dropped from the archive: the previously-copied asset is preserved.
        let staged = staged_export(&fixture_dir());
        let dir = tempdir().unwrap();
        let dest = dir.path().join("archive");

        run_import(ImportOptions {
            input_dir: staged.path().to_path_buf(),
            dest: Some(dest.clone()),
            force: false,
            incremental: false,
            fts: false,
        })
        .unwrap();

        let archived_photo = dest.join("assets/chat_001/photos/photo_1.jpg");
        assert!(
            archived_photo.is_file(),
            "first bundle import copies the photo into assets/"
        );
        let original_bytes = fs::read(&archived_photo).unwrap();

        // The re-export no longer includes the photo file (the message still
        // references it). A naive refresh rebuilds the bundle without it and would
        // lose the only archived copy.
        fs::remove_file(staged.path().join("chat_001/photos/photo_1.jpg")).unwrap();

        run_import(ImportOptions {
            input_dir: staged.path().to_path_buf(),
            dest: Some(dest.clone()),
            force: false,
            incremental: true,
            fts: false,
        })
        .unwrap();

        assert!(
            archived_photo.is_file(),
            "incremental refresh must preserve the previously archived photo"
        );
        assert_eq!(fs::read(&archived_photo).unwrap(), original_bytes);
    }

    #[test]
    fn incremental_bundle_recovers_a_stray_backup_after_a_crash() {
        // A crash between "move DEST aside to .<name>.backup-*" and "rename the new
        // bundle into DEST" strands the only copy in the backup and leaves DEST
        // missing, so a plain --incremental retry would hard-fail forever.
        // Recovery restores the lone backup so the refresh can proceed.
        let staged = staged_export(&fixture_dir());
        let dir = tempdir().unwrap();
        let dest = dir.path().join("archive");

        run_import(ImportOptions {
            input_dir: staged.path().to_path_buf(),
            dest: Some(dest.clone()),
            force: false,
            incremental: false,
            fts: false,
        })
        .unwrap();

        // Simulate the crash: DEST moved aside to a backup sibling, never renamed
        // back. The backup name matches replace_output_dir's `.<name>.backup-*`.
        let backup = dir.path().join(".archive.backup-crashsim-0");
        fs::rename(&dest, &backup).unwrap();
        assert!(!dest.exists());
        assert!(backup.join(DATABASE_FILE_NAME).is_file());

        let summary = run_import(ImportOptions {
            input_dir: staged.path().to_path_buf(),
            dest: Some(dest.clone()),
            force: false,
            incremental: true,
            fts: false,
        })
        .unwrap();

        assert!(
            dest.join(DATABASE_FILE_NAME).is_file(),
            "DEST recovered from the stray backup and refreshed"
        );
        assert!(
            !backup.exists(),
            "the stray backup was consumed by recovery, not left behind"
        );
        assert!(summary.files_imported >= 1);
    }

    pub(crate) fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic_export")
    }

    fn json_fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/json_export")
    }

    fn query_count(conn: &Connection, sql: &str) -> usize {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap() as usize
    }

    fn write_message_file(
        export_dir: &Path,
        file_name: &str,
        message_id: i64,
        sender: Option<&str>,
        joined: bool,
        text: &str,
    ) {
        let chat_dir = export_dir.join("chat_001");
        fs::create_dir_all(&chat_dir).unwrap();
        let joined_class = if joined { " joined" } else { "" };
        let sender_html = sender
            .map(|sender| format!(r#"<div class="from_name">{sender}</div>"#))
            .unwrap_or_default();
        fs::write(
            chat_dir.join(file_name),
            format!(
                r#"
                <!DOCTYPE html>
                <html>
                <body>
                 <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
                 <div class="history">
                  <div class="message default clearfix{joined_class}" id="message{message_id}">
                   <div class="body">
                    <div class="pull_right date details" title="12.02.2025 08:37:48 UTC">08:37</div>
                    {sender_html}
                    <div class="text">{text}</div>
                   </div>
                  </div>
                 </div>
                </body>
                </html>
                "#
            ),
        )
        .unwrap();
    }

    fn write_minimal_json_export(export_dir: &Path, text: &str) {
        fs::create_dir_all(export_dir).unwrap();
        fs::write(
            export_dir.join("result.json"),
            format!(
                r#"{{
                  "name": "JSON Chat",
                  "type": "personal_chat",
                  "id": 1,
                  "messages": [
                    {{
                      "id": 1,
                      "type": "message",
                      "date": "2025-01-01T00:00:00",
                      "date_unixtime": "1735689600",
                      "from": "Alice",
                      "from_id": "user1",
                      "text": "{text}"
                    }}
                  ]
                }}"#
            ),
        )
        .unwrap();
    }

    fn write_empty_messages_file(export_dir: &Path, file_name: &str) {
        let chat_dir = export_dir.join("chat_001");
        fs::create_dir_all(&chat_dir).unwrap();
        fs::write(
            chat_dir.join(file_name),
            r#"
            <!DOCTYPE html>
            <html>
            <body>
             <div class="page_header"><div class="content"><div class="text bold">Family Chat</div></div></div>
             <div class="history"></div>
            </body>
            </html>
            "#,
        )
        .unwrap();
    }
}
