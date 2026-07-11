//! Atomic output-directory replacement via a sibling temp dir and backup swap.

use crate::error::Result;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_SIBLING_DIR_ATTEMPTS: u32 = 1024;

/// Atomically replace `output_dir` with `temp_dir`.
///
/// If `output_dir` doesn't exist yet, this is a plain rename of `temp_dir`
/// into place. If it does exist, it's first moved aside to a sibling backup
/// so a failed rename can restore it; the backup is deleted once `temp_dir`
/// has been successfully swapped into `output_dir`.
pub fn replace_output_dir(temp_dir: &Path, output_dir: &Path) -> Result<()> {
    if !output_dir.exists() {
        fs::rename(temp_dir, output_dir)?;
        return Ok(());
    }

    let Some(backup_dir) = move_existing_output_to_backup(output_dir)? else {
        fs::rename(temp_dir, output_dir)?;
        return Ok(());
    };
    if let Err(error) = fs::rename(temp_dir, output_dir) {
        restore_backup_output_dir(&backup_dir, output_dir, error)?;
    } else {
        fs::remove_dir_all(&backup_dir)?;
    }
    Ok(())
}

fn restore_backup_output_dir(
    backup_dir: &Path,
    output_dir: &Path,
    original_error: io::Error,
) -> Result<()> {
    if !output_dir.exists() {
        let _ = fs::rename(backup_dir, output_dir);
    }
    Err(original_error.into())
}

/// Create a fresh, empty directory next to `output_dir` (named
/// `.<output_dir-name>.<label>-<nonce>-<n>`) for staging output before it is
/// atomically swapped into place via [`replace_output_dir`]. Retries with an
/// incrementing counter on name collisions without touching any directory it
/// didn't create.
pub fn create_sibling_work_dir(output_dir: &Path, label: &str) -> Result<PathBuf> {
    create_sibling_work_dir_with_nonce(output_dir, label, &unique_nonce())
}

fn create_sibling_work_dir_with_nonce(
    output_dir: &Path,
    label: &str,
    nonce: &str,
) -> Result<PathBuf> {
    for counter in 0..MAX_SIBLING_DIR_ATTEMPTS {
        let path = sibling_output_dir(output_dir, label, nonce, counter);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }

    Err(sibling_dir_collision_error(output_dir, label).into())
}

fn move_existing_output_to_backup(output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        return Ok(None);
    }
    move_existing_output_to_backup_with_nonce(output_dir, &unique_nonce())
}

fn move_existing_output_to_backup_with_nonce(
    output_dir: &Path,
    nonce: &str,
) -> Result<Option<PathBuf>> {
    for counter in 0..MAX_SIBLING_DIR_ATTEMPTS {
        let backup_dir = sibling_output_dir(output_dir, "backup", nonce, counter);
        if backup_dir.exists() {
            continue;
        }
        match fs::rename(output_dir, &backup_dir) {
            Ok(()) => return Ok(Some(backup_dir)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }

    Err(sibling_dir_collision_error(output_dir, "backup").into())
}

fn unique_nonce() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{timestamp}", process::id())
}

fn sibling_output_dir(output_dir: &Path, label: &str, nonce: &str, counter: u32) -> PathBuf {
    let name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("telegram-export-html");
    output_dir.with_file_name(format!(".{name}.{label}-{nonce}-{counter}"))
}

fn sibling_dir_collision_error(output_dir: &Path, label: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not allocate sibling {label} directory for {} after {MAX_SIBLING_DIR_ATTEMPTS} attempts",
            output_dir.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_sibling_work_dir_retries_without_deleting_existing_collision() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("html");
        let collision = sibling_output_dir(&output, "tmp", "fixed", 0);
        fs::create_dir(&collision).unwrap();
        fs::write(collision.join("owned.txt"), "keep").unwrap();

        let created = create_sibling_work_dir_with_nonce(&output, "tmp", "fixed").unwrap();

        assert_eq!(created, sibling_output_dir(&output, "tmp", "fixed", 1));
        assert!(collision.join("owned.txt").is_file());
        assert!(created.is_dir());
    }

    #[test]
    fn backup_rename_retries_without_deleting_existing_collision() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("html");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("stale.txt"), "old export").unwrap();
        let collision = sibling_output_dir(&output, "backup", "fixed", 0);
        fs::create_dir(&collision).unwrap();
        fs::write(collision.join("owned.txt"), "keep").unwrap();

        let backup = move_existing_output_to_backup_with_nonce(&output, "fixed")
            .unwrap()
            .unwrap();

        assert_eq!(backup, sibling_output_dir(&output, "backup", "fixed", 1));
        assert!(backup.join("stale.txt").is_file());
        assert!(collision.join("owned.txt").is_file());
        assert!(!output.exists());
    }
}
