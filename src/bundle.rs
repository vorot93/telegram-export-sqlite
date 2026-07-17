//! Post-import pass that turns a freshly built database into a self-contained
//! bundle: media is copied under `assets/`, typed attachment paths are rewritten
//! to be relative to `chat.sqlite`, and originals are preserved in
//! `extra_json["bundle"]`. Idempotent: already-relocated rows are left untouched.

use crate::{
    db,
    error::Result,
    media_copy::{MediaCopyOutcome, copy_guarded},
    media_path::safe_media_path,
    model::WarningCode,
};
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub const ASSETS_DIR: &str = "assets";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BundleMediaReport {
    pub files_copied: usize,
    pub missing: usize,
    pub unsafe_paths: usize,
}

impl BundleMediaReport {
    pub fn warnings(&self) -> usize {
        self.missing + self.unsafe_paths
    }
}

enum CopyOutcome {
    /// The file was copied; carries the actual relative path written under
    /// `assets/`, which may have been disambiguated to dodge a case-fold clash.
    Copied(String),
    /// This exact source path was already copied; carries the path it landed at
    /// (possibly disambiguated on its first copy).
    AlreadyCopied(String),
    Missing,
    /// The source resolves outside the export directory — either its
    /// canonicalized (symlink-resolved) path escapes the root, or it is a hard
    /// link (link count > 1) whose content could be an outside file. Refuse to
    /// copy it into the shareable bundle.
    Escapes,
}

/// Tracks media already copied into `assets/` during one bundling pass so the
/// same source is copied once and no two distinct sources collide on a
/// case-insensitive filesystem.
#[derive(Default)]
struct CopiedMedia {
    /// Safe source rel-path → the (possibly disambiguated) rel-path written under
    /// `assets/`. Lets a repeated reference resolve to the same written file.
    by_source: HashMap<String, String>,
    /// Lowercased written rel-paths, so a case-fold clash is detected on any
    /// filesystem (the bundle stays correct even when built on a case-sensitive
    /// one and later opened on a case-insensitive one).
    used_casefold: HashSet<String>,
}

pub fn relocate_media(
    db_path: &Path,
    source_dir: &Path,
    bundle_dir: &Path,
) -> Result<BundleMediaReport> {
    // Canonicalized once so `copy_media_file` can cheaply confirm every
    // symlink-resolved source stays inside the export dir, even when the
    // export dir itself is only reachable through a symlink (e.g. macOS
    // `/tmp` -> `/private/tmp`, `/var/folders/...`).
    let canon_root = std::fs::canonicalize(source_dir)?;
    let assets_root = bundle_dir.join(ASSETS_DIR);
    let mut copied = CopiedMedia::default();
    let mut report = BundleMediaReport::default();

    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let import_id = db::latest_import_id(&tx)?;

    for row in db::load_attachment_media(&tx)? {
        let (new_relative, orig_relative) = relocate_column(
            row.relative_path.as_deref(),
            source_dir,
            &canon_root,
            &assets_root,
            &mut copied,
            &tx,
            import_id,
            row.timeline_item_id,
            &row.extra_json,
            "source_relative_path",
            &mut report,
        )?;
        let (new_thumbnail, orig_thumbnail) = relocate_column(
            row.thumbnail_path.as_deref(),
            source_dir,
            &canon_root,
            &assets_root,
            &mut copied,
            &tx,
            import_id,
            row.timeline_item_id,
            &row.extra_json,
            "source_thumbnail_path",
            &mut report,
        )?;

        if orig_relative.is_some() || orig_thumbnail.is_some() {
            let merged = bundle_extra_json(
                &row.extra_json,
                orig_relative.as_deref(),
                orig_thumbnail.as_deref(),
            )?;
            db::update_attachment_media(
                &tx,
                row.id,
                new_relative.as_deref(),
                new_thumbnail.as_deref(),
                &merged,
            )?;
        }
    }

    // `finish_import` already wrote `imports.warnings` before this pass ran,
    // so fold in whatever this pass discovered on top of that count.
    let warning_delta = report.warnings();
    if warning_delta != 0 {
        db::add_import_warnings(&tx, import_id, warning_delta)?;
    }
    tx.commit()?;

    Ok(report)
}

/// Returns `(new_typed_value, original_value_if_rewritten)`.
#[allow(clippy::too_many_arguments)]
fn relocate_column(
    raw: Option<&str>,
    source_dir: &Path,
    canon_root: &Path,
    assets_root: &Path,
    copied: &mut CopiedMedia,
    conn: &Connection,
    import_id: i64,
    timeline_item_id: i64,
    extra_json: &str,
    bundle_key: &str,
    report: &mut BundleMediaReport,
) -> Result<(Option<String>, Option<String>)> {
    let Some(raw) = raw else {
        return Ok((None, None));
    };
    let assets_prefix = format!("{ASSETS_DIR}/");
    // Only skip as already-relocated when a prior pass actually recorded it under
    // `extra_json["bundle"][bundle_key]`. A source path that merely starts with
    // `assets/` but has no such record is genuine source media (e.g. re-importing
    // this tool's own export-html output) and must still be copied, not dropped.
    if raw.starts_with(&assets_prefix) && already_relocated(extra_json, bundle_key) {
        return Ok((Some(raw.to_string()), None));
    }
    let Some(safe) = safe_media_path(raw) else {
        report.unsafe_paths += 1;
        db::insert_media_warning(
            conn,
            import_id,
            timeline_item_id,
            WarningCode::UnsupportedMediaShape.as_str(),
            &format!("unsafe media path skipped during bundling: {raw}"),
            "{}",
        )?;
        return Ok((Some(raw.to_string()), None)); // leave unchanged
    };
    let written = match copy_media_file(source_dir, canon_root, assets_root, &safe, copied)? {
        CopyOutcome::Copied(actual) => {
            report.files_copied += 1;
            actual
        }
        CopyOutcome::AlreadyCopied(actual) => actual,
        CopyOutcome::Missing => {
            report.missing += 1;
            db::insert_media_warning(
                conn,
                import_id,
                timeline_item_id,
                WarningCode::MissingAttachment.as_str(),
                &format!("referenced media missing during bundling: {safe}"),
                "{}",
            )?;
            safe.clone() // no file to copy; point the typed path where it would be
        }
        CopyOutcome::Escapes => {
            report.unsafe_paths += 1;
            db::insert_media_warning(
                conn,
                import_id,
                timeline_item_id,
                WarningCode::UnsupportedMediaShape.as_str(),
                &format!(
                    "media path resolves outside the export directory (symlink or hard link); skipped during bundling: {safe}"
                ),
                "{}",
            )?;
            return Ok((Some(raw.to_string()), None)); // leave unchanged, symlink escape
        }
    };
    Ok((
        Some(format!("{ASSETS_DIR}/{written}")),
        Some(raw.to_string()),
    ))
}

/// True when a previous bundling pass already relocated this column, recorded by
/// the matching `extra_json["bundle"][bundle_key]`. This distinguishes our own
/// already-under-`assets/` paths (skip, for idempotent re-runs) from source media
/// that legitimately lives under `assets/` and has never been bundled (copy).
fn already_relocated(extra_json: &str, bundle_key: &str) -> bool {
    serde_json::from_str::<Value>(extra_json)
        .ok()
        .as_ref()
        .and_then(|root| root.get("bundle"))
        .and_then(|bundle| bundle.get(bundle_key))
        .is_some()
}

fn copy_media_file(
    source_dir: &Path,
    canon_root: &Path,
    assets_root: &Path,
    safe_rel: &str,
    copied: &mut CopiedMedia,
) -> Result<CopyOutcome> {
    if let Some(actual) = copied.by_source.get(safe_rel) {
        return Ok(CopyOutcome::AlreadyCopied(actual.clone()));
    }
    let source = source_dir.join(safe_rel);
    // Pick a written path that does not case-fold onto one already used, so a
    // second distinct source (`Report.pdf` vs `report.pdf`) cannot clobber the
    // first when the bundle is opened on a case-insensitive filesystem. This only
    // reserves the name if the copy actually succeeds below.
    let written = disambiguated_path(safe_rel, &copied.used_casefold);
    let target = assets_root.join(&written);
    // The symlink/hard-link/inside-root guards live in `media_copy::copy_guarded`,
    // shared with HTML export so both apply the identical anti-smuggling policy.
    match copy_guarded(canon_root, &source, &target)? {
        MediaCopyOutcome::Copied => {
            copied.used_casefold.insert(written.to_lowercase());
            copied
                .by_source
                .insert(safe_rel.to_string(), written.clone());
            Ok(CopyOutcome::Copied(written))
        }
        MediaCopyOutcome::Missing => Ok(CopyOutcome::Missing),
        MediaCopyOutcome::Escapes => Ok(CopyOutcome::Escapes),
    }
}

/// A relative path whose lowercase is not already in `used_casefold`. Returns
/// `relative` unchanged when there is no clash; otherwise inserts `_2`, `_3`, …
/// before the extension (`dir/name.ext` → `dir/name_2.ext`). Keeps forward
/// slashes, since bundle rel-paths are always forward-slash.
fn disambiguated_path(relative: &str, used_casefold: &HashSet<String>) -> String {
    if !used_casefold.contains(&relative.to_lowercase()) {
        return relative.to_string();
    }
    let (dir, file) = match relative.rsplit_once('/') {
        Some((dir, file)) => (Some(dir), file),
        None => (None, relative),
    };
    let (stem, ext) = match file.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (file, None),
    };
    for suffix in 2..=u32::MAX {
        let candidate_file = match ext {
            Some(ext) => format!("{stem}_{suffix}.{ext}"),
            None => format!("{stem}_{suffix}"),
        };
        let candidate = match dir {
            Some(dir) => format!("{dir}/{candidate_file}"),
            None => candidate_file,
        };
        if !used_casefold.contains(&candidate.to_lowercase()) {
            return candidate;
        }
    }
    relative.to_string() // unreachable: 4 billion collisions on one name
}

fn bundle_extra_json(
    extra_json: &str,
    source_relative: Option<&str>,
    source_thumbnail: Option<&str>,
) -> Result<String> {
    // `extra_json` is `NOT NULL DEFAULT '{}'`, so a parse failure here should
    // never happen in practice. But the project's fidelity rules forbid
    // silently dropping data (see AGENTS.md), so a non-parseable value is
    // preserved verbatim under a raw-string key instead of being replaced
    // with `{}`.
    let mut root: Value = serde_json::from_str(extra_json).unwrap_or_else(|_| {
        let mut wrapper = Map::new();
        wrapper.insert(
            "bundle_source_extra_raw".to_string(),
            Value::String(extra_json.to_string()),
        );
        Value::Object(wrapper)
    });
    if !root.is_object() {
        let mut wrapper = Map::new();
        wrapper.insert("source_extra".to_string(), root);
        root = Value::Object(wrapper);
    }
    let obj = root.as_object_mut().expect("root normalized to object");

    let mut bundle = Map::new();
    if let Some(rel) = source_relative {
        bundle.insert(
            "source_relative_path".to_string(),
            Value::String(rel.to_string()),
        );
    }
    if let Some(thumb) = source_thumbnail {
        bundle.insert(
            "source_thumbnail_path".to_string(),
            Value::String(thumb.to_string()),
        );
    }
    match obj.get_mut("bundle").and_then(Value::as_object_mut) {
        Some(existing) => {
            for (key, value) in bundle {
                existing.insert(key, value);
            }
        }
        None => {
            obj.insert("bundle".to_string(), Value::Object(bundle));
        }
    }
    Ok(serde_json::to_string(&root)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_extra_json_preserves_existing_keys() {
        let merged = bundle_extra_json(
            "{\"source_json\":{\"k\":1},\"href\":\"photos/p.jpg\"}",
            Some("photos/p.jpg"),
            None,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["source_json"]["k"], 1);
        assert_eq!(value["href"], "photos/p.jpg");
        assert_eq!(value["bundle"]["source_relative_path"], "photos/p.jpg");
    }

    #[test]
    fn bundle_extra_json_extends_existing_bundle_object() {
        let merged = bundle_extra_json(
            "{\"source_json\":{\"k\":1},\"bundle\":{\"source_thumbnail_path\":\"chat_001/photos/t.jpg\"}}",
            Some("chat_001/photos/p.jpg"),
            None,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["source_json"]["k"], 1);
        assert_eq!(
            value["bundle"]["source_thumbnail_path"],
            "chat_001/photos/t.jpg"
        );
        assert_eq!(
            value["bundle"]["source_relative_path"],
            "chat_001/photos/p.jpg"
        );
    }

    fn imported_fixture_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let staged = crate::importer::tests::staged_export(&crate::importer::tests::fixture_dir());
        crate::importer::run_import(crate::importer::tests::import_options(
            staged.path(),
            true,
            false,
            false,
        ))
        .unwrap();
        let db = staged.path().join(crate::importer::DATABASE_FILE_NAME);
        (staged, db)
    }

    #[test]
    fn relocate_media_copies_and_rewrites_paths() {
        let (staged, db) = imported_fixture_db();
        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.files_copied >= 2);
        // The fixture's basic_export nests media under chat_001/ (the parser
        // stores attachment paths relative to the staged input_dir, prefixed
        // with the per-chat directory); the bundle preserves that full
        // relative path under assets/ rather than flattening it, so that
        // merged multi-chat bundles (see `merge`) can't collide on filenames
        // that repeat across chats (e.g. two chats each with photos/photo_1.jpg).
        assert!(
            bundle_dir
                .path()
                .join("assets/chat_001/photos/photo_1.jpg")
                .is_file()
        );
        assert!(
            bundle_dir
                .path()
                .join("assets/chat_001/files/report.pdf")
                .is_file()
        );

        let conn = Connection::open(&db).unwrap();
        let rows = db::load_attachment_media(&conn).unwrap();
        assert!(rows.iter().any(|r| {
            r.relative_path
                .as_deref()
                .is_some_and(|p| p.starts_with("assets/"))
        }));
        let value: Value = serde_json::from_str(&rows[0].extra_json).unwrap();
        assert!(value.get("bundle").is_some());

        // Idempotent: a second pass copies nothing further and does not double-prefix.
        let second = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();
        assert_eq!(second.files_copied, 0);
        let rows = db::load_attachment_media(&Connection::open(&db).unwrap()).unwrap();
        assert!(rows.iter().all(|r| {
            r.relative_path
                .as_deref()
                .is_none_or(|p| !p.starts_with("assets/assets/"))
        }));
    }

    #[test]
    fn relocate_media_copies_source_media_under_assets_without_a_bundle_record() {
        let (staged, db) = imported_fixture_db();

        // Point one attachment at media that legitimately lives under `assets/`
        // with NO bundle record -- the shape you get re-importing this tool's own
        // export-html output. The old code saw the `assets/` prefix, assumed the
        // row was already relocated, and skipped the copy, silently losing media.
        let conn = Connection::open(&db).unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM attachments WHERE relative_path IS NOT NULL ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE attachments SET relative_path = 'assets/report.pdf' WHERE id = ?1",
            [id],
        )
        .unwrap();
        drop(conn);
        std::fs::create_dir_all(staged.path().join("assets")).unwrap();
        std::fs::write(staged.path().join("assets/report.pdf"), b"pdf bytes").unwrap();

        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.files_copied >= 1);
        assert!(
            bundle_dir.path().join("assets/assets/report.pdf").is_file(),
            "source media under assets/ with no bundle record must be copied, not skipped"
        );
    }

    #[test]
    fn relocate_media_disambiguates_case_folded_filename_collisions() {
        let (staged, db) = imported_fixture_db();

        // Two attachments whose paths differ only by case. On a case-insensitive
        // filesystem they are one file, so the second fs::copy would clobber the
        // first. The bundle must keep BOTH under distinct on-disk names so it is
        // portable to macOS/Windows regardless of where it was built.
        let conn = Connection::open(&db).unwrap();
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM attachments WHERE relative_path IS NOT NULL ORDER BY id LIMIT 2",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(ids.len(), 2, "fixture needs two media attachments");
        conn.execute(
            "UPDATE attachments SET relative_path = 'Report.pdf', thumbnail_path = NULL WHERE id = ?1",
            [ids[0]],
        )
        .unwrap();
        conn.execute(
            "UPDATE attachments SET relative_path = 'report.pdf', thumbnail_path = NULL WHERE id = ?1",
            [ids[1]],
        )
        .unwrap();
        drop(conn);
        std::fs::write(staged.path().join("Report.pdf"), b"UPPER").unwrap();
        std::fs::write(staged.path().join("report.pdf"), b"lower").unwrap();

        let bundle_dir = tempfile::tempdir().unwrap();
        relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        let conn = Connection::open(&db).unwrap();
        let paths: Vec<String> = conn
            .prepare("SELECT relative_path FROM attachments WHERE id IN (?1, ?2) ORDER BY id")
            .unwrap()
            .query_map([ids[0], ids[1]], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        // The two stored paths must be case-insensitively DISTINCT.
        assert_ne!(
            paths[0].to_lowercase(),
            paths[1].to_lowercase(),
            "case-folded collision must be disambiguated: {paths:?}"
        );
        // Both files exist in the bundle with their own (distinct) content.
        assert_eq!(
            std::fs::read(bundle_dir.path().join(&paths[0])).unwrap(),
            b"UPPER"
        );
        assert_eq!(
            std::fs::read(bundle_dir.path().join(&paths[1])).unwrap(),
            b"lower"
        );
    }

    #[test]
    fn relocate_media_warns_on_missing_media_but_still_rewrites() {
        let (staged, db) = imported_fixture_db();
        std::fs::remove_file(staged.path().join("chat_001/photos/photo_1.jpg")).unwrap();

        // Baseline `imports.warnings` written by `finish_import` at import time,
        // captured before the bundling pass so the assertion below can be exact.
        let warnings_before: i64 = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT warnings FROM imports ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.missing >= 1);
        assert!(report.warnings() >= 1);
        let conn = Connection::open(&db).unwrap();
        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'missing_attachment'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 1);
        // Confirms the row whose source file was deleted still had its typed
        // path rewritten to assets/, despite the file being missing.
        // Searching with `.any(...)` over every row is still precise here
        // because this rewritten path string is unique to this attachment in
        // the fixture, so a match can only be the row we deleted the file for.
        let rows = db::load_attachment_media(&conn).unwrap();
        assert!(
            rows.iter()
                .any(|r| r.relative_path.as_deref() == Some("assets/chat_001/photos/photo_1.jpg"))
        );

        // `finish_import` wrote `imports.warnings` before this bundling pass ran,
        // so the column must equal the baseline PLUS the pass's own warning count.
        // Exact equality (not `>=`, which the fixture baseline satisfies trivially)
        // is what actually catches a reverted `add_import_warnings` wiring.
        let import_id: i64 = conn
            .query_row("SELECT id FROM imports ORDER BY id DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let recorded_warnings: i64 = conn
            .query_row(
                "SELECT warnings FROM imports WHERE id = ?1",
                [import_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            recorded_warnings,
            warnings_before + i64::try_from(report.warnings()).unwrap()
        );
    }

    #[test]
    fn relocate_media_skips_and_warns_on_unsafe_path() {
        let (staged, db) = imported_fixture_db();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE attachments SET relative_path = '/etc/passwd'
                 WHERE id = (SELECT MIN(id) FROM attachments)",
                [],
            )
            .unwrap();
        }

        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.unsafe_paths >= 1);
        let conn = Connection::open(&db).unwrap();
        let unchanged: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachments WHERE relative_path = '/etc/passwd'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, 1); // left unprefixed
        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'unsupported_media_shape'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn relocate_media_skips_symlink_escaping_export() {
        use std::os::unix::fs::symlink;

        let (staged, db) = imported_fixture_db();
        let outside = tempfile::tempdir().unwrap();
        let secret_path = outside.path().join("secret.jpg");
        std::fs::write(&secret_path, b"top secret bytes").unwrap();

        // Replace a real, referenced media file with a symlink that resolves
        // outside the export directory, the way a crafted export could try to
        // smuggle an arbitrary file into a shared bundle.
        let target = staged.path().join("chat_001/photos/photo_1.jpg");
        std::fs::remove_file(&target).unwrap();
        symlink(&secret_path, &target).unwrap();

        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.unsafe_paths >= 1);

        let conn = Connection::open(&db).unwrap();
        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'unsupported_media_shape'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 1);

        // Typed path for the symlinked attachment is left unprefixed, mirroring
        // the unsafe_media_path-reject branch.
        let rows = db::load_attachment_media(&conn).unwrap();
        assert!(
            rows.iter()
                .any(|r| { r.relative_path.as_deref() == Some("chat_001/photos/photo_1.jpg") })
        );

        // The secret must not have leaked into the bundle's assets/ dir.
        assert!(
            !bundle_dir
                .path()
                .join("assets/chat_001/photos/photo_1.jpg")
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn relocate_media_skips_hard_link_escaping_export() {
        let (staged, db) = imported_fixture_db();
        let outside = tempfile::tempdir().unwrap();
        let secret_path = outside.path().join("secret.jpg");
        std::fs::write(&secret_path, b"top secret bytes").unwrap();

        // Replace a real, referenced media file with a HARD LINK to an outside
        // file. Unlike a symlink, its canonical path stays inside the export, so
        // canonicalize + `starts_with` cannot catch it; the link count can. A
        // crafted export could otherwise smuggle an arbitrary file (e.g. a
        // private key) into a bundle the user then shares.
        let target = staged.path().join("chat_001/photos/photo_1.jpg");
        std::fs::remove_file(&target).unwrap();
        std::fs::hard_link(&secret_path, &target).unwrap();

        let bundle_dir = tempfile::tempdir().unwrap();
        let report = relocate_media(&db, staged.path(), bundle_dir.path()).unwrap();

        assert!(report.unsafe_paths >= 1);

        let conn = Connection::open(&db).unwrap();
        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'unsupported_media_shape'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 1);

        // Typed path for the hard-linked attachment is left unprefixed.
        let rows = db::load_attachment_media(&conn).unwrap();
        assert!(
            rows.iter()
                .any(|r| { r.relative_path.as_deref() == Some("chat_001/photos/photo_1.jpg") })
        );

        // The secret must not have leaked into the bundle's assets/ dir.
        assert!(
            !bundle_dir
                .path()
                .join("assets/chat_001/photos/photo_1.jpg")
                .is_file()
        );
    }
}
