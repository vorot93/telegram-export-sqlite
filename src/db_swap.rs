//! Atomic single-file database replacement via a temporary sibling file.
//!
//! The file-level analog of [`crate::output_dir`]'s directory-level swap:
//! `import` and `merge` build into a temporary sibling database, then
//! atomically move it into place, cleaning up the temp file and its SQLite
//! sidecars afterward.

use crate::error::Result;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// A hidden sibling path next to `output_db` for building a replacement
/// database before the atomic swap. Unique per process and nanosecond.
pub fn temporary_database_path(output_db: &Path) -> PathBuf {
    let parent = output_db
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = output_db
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "database.sqlite".into());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    parent.join(format!(
        ".{file_name}.tmp-{}-{timestamp}",
        std::process::id()
    ))
}

#[cfg(unix)]
pub fn replace_database(temp_path: &Path, output_db: &Path) -> Result<()> {
    fs::rename(temp_path, output_db)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn replace_database(temp_path: &Path, output_db: &Path) -> Result<()> {
    let backup_path = temporary_backup_path(output_db);
    if output_db.exists() {
        fs::rename(output_db, &backup_path)?;
    }
    if let Err(error) = fs::rename(temp_path, output_db) {
        if backup_path.exists() {
            let _ = fs::rename(&backup_path, output_db);
        }
        return Err(error.into());
    }
    cleanup_temp_database(&backup_path);
    Ok(())
}

#[cfg(not(unix))]
fn temporary_backup_path(output_db: &Path) -> PathBuf {
    let mut backup_path = temporary_database_path(output_db);
    backup_path.set_extension("backup");
    backup_path
}

/// Remove a temporary database file and its SQLite sidecars (`-journal`,
/// `-wal`, `-shm`). Missing files are ignored.
pub fn cleanup_temp_database(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-journal", "-wal", "-shm"] {
        let _ = fs::remove_file(path_with_suffix(path, suffix));
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::{cleanup_temp_database, path_with_suffix, temporary_database_path};
    use std::{fs, path::Path};
    use tempfile::tempdir;

    #[test]
    fn temporary_path_is_hidden_sibling_of_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("chat.sqlite");
        let temp = temporary_database_path(&target);
        assert_eq!(temp.parent(), Some(dir.path()));
        let name = temp.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with(".chat.sqlite.tmp-"),
            "unexpected temp name: {name}"
        );
    }

    #[test]
    fn temporary_path_falls_back_to_current_dir_for_bare_name() {
        let temp = temporary_database_path(Path::new("chat.sqlite"));
        assert_eq!(temp.parent(), Some(Path::new(".")));
        assert!(
            temp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".chat.sqlite.tmp-")
        );
    }

    #[test]
    fn cleanup_removes_target_and_sqlite_sidecars_and_ignores_missing() {
        let dir = tempdir().unwrap();
        let db = dir.path().join(".chat.sqlite.tmp-xyz");
        fs::write(&db, b"db").unwrap();
        for suffix in ["-journal", "-wal", "-shm"] {
            fs::write(path_with_suffix(&db, suffix), b"side").unwrap();
        }

        cleanup_temp_database(&db);

        assert!(!db.exists());
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(!path_with_suffix(&db, suffix).exists());
        }
        // A second call over already-removed files must not panic.
        cleanup_temp_database(&db);
    }
}
