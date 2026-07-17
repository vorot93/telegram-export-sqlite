//! Guarded copy of a single referenced media file into an output directory,
//! shared by bundle creation ([`crate::bundle`]) and HTML export
//! ([`crate::html_export`]). It enforces that the source is a real regular file
//! inside the canonicalized export root and is not a hard link (whose content
//! could be an outside file), so neither a shareable bundle nor an exported HTML
//! tree can be tricked by a crafted export into embedding an arbitrary file.

use crate::error::Result;
use std::{fs, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub enum MediaCopyOutcome {
    Copied,
    /// The source does not exist, is a broken symlink, or is not a regular file.
    Missing,
    /// The source canonicalizes outside the export root (a symlink pointing away)
    /// or is a hard link (link count > 1) whose content could be an outside file.
    /// Refused rather than copied into shareable output.
    Escapes,
}

/// Copy `source` to `target`, creating any missing parent directories, but only
/// when `source` canonicalizes to a regular file inside `canon_root` and is not a
/// hard link. `canon_root` must already be canonicalized by the caller (once per
/// pass), so a source reachable only through a symlinked export root still matches.
///
/// Symlinks are resolved before the file is trusted: copying the un-resolved path
/// would follow a symlink anywhere on disk, letting a crafted export smuggle an
/// arbitrary file into the output. A path that fails to canonicalize (absent file
/// or broken symlink) is reported as [`MediaCopyOutcome::Missing`].
pub fn copy_guarded(canon_root: &Path, source: &Path, target: &Path) -> Result<MediaCopyOutcome> {
    let canon_source = match fs::canonicalize(source) {
        Ok(path) => path,
        Err(_) => return Ok(MediaCopyOutcome::Missing),
    };
    if !canon_source.starts_with(canon_root) {
        return Ok(MediaCopyOutcome::Escapes);
    }
    let Ok(meta) = fs::metadata(&canon_source) else {
        return Ok(MediaCopyOutcome::Missing);
    };
    if !meta.is_file() {
        return Ok(MediaCopyOutcome::Missing);
    }
    // A hard link's canonical path stays inside the export even when its content is
    // an outside file, so the `starts_with` check above cannot catch it — but the
    // link count can. Unix-only; a legitimately hard-linked media file is rare and
    // the caller degrades it to a warning with the path left unchanged.
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::nlink(&meta) > 1 {
        return Ok(MediaCopyOutcome::Escapes);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&canon_source, target)?;
    Ok(MediaCopyOutcome::Copied)
}
