use crate::{
    error::{Result, TelegramExportError},
    model::SourceFile,
};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn discover_json_export_file(input_dir: &Path) -> Result<Option<SourceFile>> {
    if !input_dir.is_dir() {
        return Err(TelegramExportError::InputDirectoryMissing(
            input_dir.to_path_buf(),
        ));
    }

    let absolute_path = input_dir.join("result.json");
    if !absolute_path.is_file() {
        return Ok(None);
    }

    source_file_from_path(input_dir, absolute_path, 0).map(Some)
}

pub fn discover_messages_files(input_dir: &Path) -> Result<Vec<SourceFile>> {
    if !input_dir.is_dir() {
        return Err(TelegramExportError::InputDirectoryMissing(
            input_dir.to_path_buf(),
        ));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(input_dir) {
        let entry = entry.map_err(walkdir_error)?;
        // `entry.path().is_file()` follows symlinks, so a split file kept as a symlink is
        // discovered rather than silently skipped (matching result.json discovery, which
        // uses Path::is_file); directory symlinks and broken links still resolve to false.
        if !entry.path().is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy();
        if is_message_split_file(&name) && !within_media_subdir(input_dir, entry.path()) {
            candidates.push(entry.path().to_path_buf());
        }
    }

    candidates.sort_by(|left, right| compare_message_paths(input_dir, left, right));

    if candidates.is_empty() {
        return Err(TelegramExportError::NoMessagesFiles(
            input_dir.to_path_buf(),
        ));
    }

    candidates
        .into_iter()
        .enumerate()
        .map(|(parse_order, absolute_path)| {
            source_file_from_path(input_dir, absolute_path, parse_order)
        })
        .collect()
}

fn source_file_from_path(
    input_dir: &Path,
    absolute_path: PathBuf,
    parse_order: usize,
) -> Result<SourceFile> {
    let source_path = std::fs::canonicalize(&absolute_path)?;
    let metadata = std::fs::metadata(&source_path)?;
    Ok(SourceFile {
        relative_path: relative_path(input_dir, &absolute_path),
        checksum: sha256_file(&source_path)?,
        file_size: metadata.len(),
        absolute_path: source_path,
        parse_order,
    })
}

fn relative_path(base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}

fn compare_message_paths(base: &Path, left: &Path, right: &Path) -> std::cmp::Ordering {
    let left_relative = relative_path(base, left);
    let right_relative = relative_path(base, right);
    let left_parent = left_relative.parent().unwrap_or_else(|| Path::new(""));
    let right_parent = right_relative.parent().unwrap_or_else(|| Path::new(""));

    left_parent
        .cmp(right_parent)
        .then_with(|| {
            message_split_index(&left_relative).cmp(&message_split_index(&right_relative))
        })
        .then_with(|| left_relative.cmp(&right_relative))
}

fn message_split_index(path: &Path) -> u64 {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return u64::MAX;
    };
    let Some(stem) = file_name.strip_suffix(".html") else {
        return u64::MAX;
    };
    let Some(suffix) = stem.strip_prefix("messages") else {
        return u64::MAX;
    };

    if suffix.is_empty() {
        1
    } else {
        suffix.parse::<u64>().unwrap_or(u64::MAX)
    }
}

/// A genuine Telegram Desktop split-message file name: exactly `messages.html`
/// or `messages{N}.html` with N a plain decimal ≥ 2. tdesktop's
/// `HtmlWriter::messagesFile` emits `"messages" + (index>0 ? number(index+1) :
/// "") + ".html"`, so index 0 → `messages.html`, index 1 → `messages2.html`, … —
/// there is no `messages1.html` or `messages0.html`. This rejects chat-supplied
/// files like `messages_notes.html` that only coincidentally start with
/// "messages".
fn is_message_split_file(name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix("messages")
        .and_then(|rest| rest.strip_suffix(".html"))
    else {
        return false;
    };
    suffix.is_empty()
        || (!suffix.starts_with('0')
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
            && suffix.parse::<u64>().is_ok_and(|index| index >= 2))
}

/// Directories a Telegram Desktop export writes chat media into (`DocumentFolder`
/// plus the photo/userpic/story/contact/music folders). Split message files are
/// direct children of their chat directory and never live inside these, so a
/// `messages*.html` found under one is a chat-supplied file masquerading as a
/// split page. Chat directories are named `chat_N`/`chats`, never these, so the
/// check never rejects a genuine split file.
const MEDIA_SUBDIRS: &[&str] = &[
    "photos",
    "video_files",
    "voice_messages",
    "round_video_messages",
    "animations",
    "stickers",
    "files",
    "contacts",
    "profile_pictures",
    "stories",
    "music",
];

fn within_media_subdir(base: &Path, path: &Path) -> bool {
    let relative = relative_path(base, path);
    let Some(parent) = relative.parent() else {
        return false;
    };
    parent.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| MEDIA_SUBDIRS.contains(&name))
    })
}

fn walkdir_error(error: walkdir::Error) -> TelegramExportError {
    let message = error.to_string();
    let error = error
        .into_io_error()
        .unwrap_or_else(|| std::io::Error::other(message));
    TelegramExportError::Io(error)
}

fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex_digest(hasher.finalize()))
}

/// SHA-256 of in-memory bytes, hex-encoded. Used by the parsers to checksum the
/// exact content they parse, so the stored checksum always matches the imported
/// content even if the file changes between discovery-time hashing and the
/// parser's read (see the discovery/parse TOCTOU note in AGENTS.md).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_digest(hasher.finalize())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut checksum = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to String cannot fail");
    }
    checksum
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn detects_messages_html_files_in_stable_order() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("chat_002")).unwrap();
        fs::create_dir_all(dir.path().join("chat_001")).unwrap();
        fs::write(dir.path().join("chat_002/messages2.html"), "two").unwrap();
        fs::write(dir.path().join("chat_001/messages.html"), "one").unwrap();
        fs::write(dir.path().join("chat_001/index.html"), "ignored").unwrap();

        let files = discover_messages_files(dir.path()).unwrap();

        let relative: Vec<_> = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect();
        assert_eq!(
            relative,
            vec![
                PathBuf::from("chat_001").join("messages.html"),
                PathBuf::from("chat_002").join("messages2.html")
            ]
        );
        assert_eq!(files[0].parse_order, 0);
        assert_eq!(files[1].parse_order, 1);
    }

    #[test]
    fn sorts_telegram_split_files_naturally() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("chat_001")).unwrap();
        fs::write(dir.path().join("chat_001/messages10.html"), "ten").unwrap();
        fs::write(dir.path().join("chat_001/messages2.html"), "two").unwrap();
        fs::write(dir.path().join("chat_001/messages.html"), "one").unwrap();

        let files = discover_messages_files(dir.path()).unwrap();

        let relative: Vec<_> = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect();
        assert_eq!(
            relative,
            vec![
                PathBuf::from("chat_001").join("messages.html"),
                PathBuf::from("chat_001").join("messages2.html"),
                PathBuf::from("chat_001").join("messages10.html")
            ]
        );
        assert_eq!(files[0].parse_order, 0);
        assert_eq!(files[1].parse_order, 1);
        assert_eq!(files[2].parse_order, 2);
    }

    #[test]
    fn ignores_messages_html_inside_media_subdirectories() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("messages.html"), "real").unwrap();
        // A chat participant sent a file literally named messages2.html; Telegram
        // Desktop stores sent documents under files/. It must NOT be discovered
        // and parsed as a split page (it would inject forged timeline rows).
        fs::create_dir_all(dir.path().join("files")).unwrap();
        fs::write(dir.path().join("files/messages2.html"), "<forged/>").unwrap();
        // Same for a chat-nested layout: media lives under chat_001/photos/.
        fs::create_dir_all(dir.path().join("chat_001/photos")).unwrap();
        fs::write(dir.path().join("chat_001/messages.html"), "real2").unwrap();
        fs::write(
            dir.path().join("chat_001/photos/messages3.html"),
            "<forged/>",
        )
        .unwrap();

        let files = discover_messages_files(dir.path()).unwrap();
        let rel: Vec<_> = files.iter().map(|f| f.relative_path.clone()).collect();

        assert!(rel.contains(&PathBuf::from("messages.html")));
        assert!(rel.contains(&PathBuf::from("chat_001").join("messages.html")));
        assert!(
            !rel.iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "files")),
            "a messages*.html under files/ must be ignored: {rel:?}"
        );
        assert!(
            !rel.iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "photos")),
            "a messages*.html under photos/ must be ignored: {rel:?}"
        );
    }

    #[test]
    fn ignores_non_tdesktop_message_file_names() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("messages.html"), "real").unwrap();
        fs::write(dir.path().join("messages2.html"), "real2").unwrap();
        // tdesktop only ever emits messages.html, messages2.html, messages3.html …
        // (index 0 -> no suffix, index N>0 -> N+1). None of these are genuine.
        fs::write(dir.path().join("messages_notes.html"), "<forged/>").unwrap();
        fs::write(dir.path().join("messages1.html"), "<forged/>").unwrap();
        fs::write(dir.path().join("messages0.html"), "<forged/>").unwrap();

        let files = discover_messages_files(dir.path()).unwrap();
        let rel: Vec<_> = files.iter().map(|f| f.relative_path.clone()).collect();

        assert_eq!(
            rel,
            vec![
                PathBuf::from("messages.html"),
                PathBuf::from("messages2.html")
            ],
            "only genuine tdesktop split-file names are discovered"
        );
    }

    #[test]
    fn computes_sha256_checksum() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("messages.html"), "abc").unwrap();

        let files = discover_messages_files(dir.path()).unwrap();

        assert_eq!(
            files[0].checksum,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(files[0].file_size, 3);
    }

    #[test]
    fn makes_source_paths_absolute_for_relative_input_dir() {
        let dir = tempfile::Builder::new()
            .prefix("telegram_export_relative_")
            .tempdir_in(".")
            .unwrap();
        fs::create_dir_all(dir.path().join("chat_001")).unwrap();
        fs::write(dir.path().join("chat_001/messages.html"), "one").unwrap();

        let input_dir = PathBuf::from(dir.path().file_name().unwrap());

        let files = discover_messages_files(&input_dir).unwrap();

        assert!(files.iter().all(|file| file.absolute_path.is_absolute()));
        assert_eq!(
            files[0].relative_path,
            PathBuf::from("chat_001").join("messages.html")
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovers_symlinked_message_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("messages.html"), "one").unwrap();
        // A split file kept as a symlink (e.g. into deduplicated storage) must not be
        // silently dropped; source_file_from_path already canonicalizes it downstream.
        let target = dir.path().join("stored_two.html");
        fs::write(&target, "two").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("messages2.html")).unwrap();

        let files = discover_messages_files(dir.path()).unwrap();

        let relative: Vec<_> = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect();
        assert!(relative.contains(&PathBuf::from("messages.html")));
        assert!(
            relative.contains(&PathBuf::from("messages2.html")),
            "symlinked split file must be discovered: {relative:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn propagates_directory_traversal_errors() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
        }

        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
            }
        }

        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("chat_001")).unwrap();
        fs::write(dir.path().join("chat_001/messages.html"), "one").unwrap();
        let unreadable = dir.path().join("unreadable");
        fs::create_dir_all(&unreadable).unwrap();
        let _restore_permissions = RestorePermissions {
            path: unreadable.clone(),
        };
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let error = discover_messages_files(dir.path()).unwrap_err();

        assert!(matches!(error, TelegramExportError::Io(_)));
    }
}
