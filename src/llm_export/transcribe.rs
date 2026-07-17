//! Optional voice / video-note transcription for `export-llm`.
//!
//! A user-supplied command is parsed into an argv and executed **directly** via
//! `std::process::Command` — never through a shell — so a media filename can
//! never inject shell metacharacters (`safe_media_path` permits spaces and
//! shell-special characters in path components). See the design spec §5.

use crate::{
    error::{Result, TelegramExportError},
    export_rows::ExportRows,
    media_path::safe_media_path,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
};

/// Attachment kinds eligible for transcription: voice notes (HTML `voice`,
/// JSON `voice_message`) and round video notes (JSON `video_message`).
fn is_transcribable(kind: &str) -> bool {
    matches!(kind, "voice" | "voice_message" | "video_message")
}

/// A parsed `--transcribe` command template: an argv with optional `{}`
/// placeholders.
#[derive(Debug, Clone)]
pub(crate) struct TranscribeCommand {
    argv: Vec<String>,
    has_placeholder: bool,
}

impl TranscribeCommand {
    /// Parse a command string into a quote-aware argv template. Errors on
    /// unbalanced quotes or an empty command.
    pub(crate) fn parse(command: &str) -> Result<Self> {
        let argv = shell_words::split(command)
            .map_err(|e| TelegramExportError::TranscribeCommandInvalid(e.to_string()))?;
        if argv.is_empty() {
            return Err(TelegramExportError::TranscribeCommandInvalid(
                "command is empty".to_string(),
            ));
        }
        let has_placeholder = argv.iter().any(|arg| arg == "{}");
        Ok(Self {
            argv,
            has_placeholder,
        })
    }

    /// Concrete argv for one audio file. Every element equal to exactly `{}`
    /// becomes `path`; if none is, `path` is appended as the final argument.
    /// Elements are `OsString` so a non-UTF-8 path is passed through verbatim
    /// rather than lossily replaced.
    fn argv_for(&self, path: &OsStr) -> Vec<OsString> {
        if self.has_placeholder {
            self.argv
                .iter()
                .map(|arg| {
                    if arg == "{}" {
                        path.to_os_string()
                    } else {
                        OsString::from(arg)
                    }
                })
                .collect()
        } else {
            let mut argv: Vec<OsString> = self.argv.iter().map(OsString::from).collect();
            argv.push(path.to_os_string());
            argv
        }
    }
}

/// Outcome of a transcription pass.
#[derive(Debug, Default)]
pub(crate) struct TranscriptionResult {
    /// Attachment `relative_path` (verbatim) → non-empty transcript.
    pub transcripts: HashMap<String, String>,
    pub transcribed: usize,
    pub failed: usize,
}

/// Transcribe every in-scope attachment. `db_dir` is `dirname(INPUT_DB)`; media
/// resolves to `db_dir + safe_media_path(relative_path)`, canonicalized (which
/// also confirms the file exists). Any per-file failure increments `failed` and
/// is skipped — one bad file never aborts the pass.
pub(crate) fn transcribe_attachments(
    export: &ExportRows,
    db_dir: &Path,
    command: &TranscribeCommand,
) -> TranscriptionResult {
    let mut result = TranscriptionResult::default();
    // Resolve the export dir so we can reject media whose real (symlink-resolved)
    // path escapes it — the same protection bundle.rs applies when copying media.
    let canon_db_dir = std::fs::canonicalize(db_dir).ok();
    // Each distinct media file is processed at most once: several attachment rows
    // can reference the same `relative_path` (a re-merged DB, a repeatedly quoted
    // voice note), and the engine call may be slow or paid. The transcript map is
    // keyed by `relative_path`, so every referencing row still resolves it.
    let mut seen: HashSet<&str> = HashSet::new();
    for att in &export.attachments {
        if !is_transcribable(&att.attachment_kind) || att.skip_reason.is_some() {
            continue;
        }
        let Some(rel) = att.relative_path.as_deref() else {
            continue;
        };
        if !seen.insert(rel) {
            continue; // already processed this file — don't pay for it again
        }
        let Some(safe) = safe_media_path(rel) else {
            result.failed += 1;
            continue;
        };
        let Ok(absolute) = std::fs::canonicalize(db_dir.join(&safe)) else {
            result.failed += 1; // missing / unreadable file
            continue;
        };
        // Fail closed: only proceed when the file provably resolves inside the
        // export root. If the root itself couldn't be canonicalized we can't
        // prove containment, so treat the file as an escape rather than trust it.
        let inside = canon_db_dir
            .as_deref()
            .is_some_and(|root| absolute.starts_with(root));
        if !inside {
            result.failed += 1; // resolves outside the export dir (symlink escape)
            continue;
        }
        // A hard link's canonical path stays inside the export even when its
        // content is an outside file, so `starts_with` cannot catch it — but a
        // link count other than 1 can. Refuse it rather than pipe an arbitrary
        // file (e.g. `~/.ssh/id_rsa`) to the external engine. Unix-only; a
        // legitimately hard-linked media file is rare and degrades to the bare
        // placeholder.
        #[cfg(unix)]
        if std::fs::metadata(&absolute)
            .map(|meta| std::os::unix::fs::MetadataExt::nlink(&meta))
            .unwrap_or(0)
            != 1
        {
            result.failed += 1; // hard-linked file may point outside the export
            continue;
        }
        match run_engine(command, &absolute) {
            Some(text) if !text.is_empty() => {
                result.transcripts.insert(rel.to_string(), text);
                result.transcribed += 1;
            }
            _ => result.failed += 1,
        }
    }
    result
}

/// Run the engine on one file; return trimmed stdout, or `None` on any failure
/// (spawn error or non-zero exit).
fn run_engine(command: &TranscribeCommand, path: &Path) -> Option<String> {
    let argv = command.argv_for(path.as_os_str());
    let output = Command::new(&argv[0]).args(&argv[1..]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_rows::AttachmentRow;

    fn voice_attachment(kind: &str, rel: &str) -> AttachmentRow {
        AttachmentRow {
            timeline_item_id: 1,
            attachment_kind: kind.to_string(),
            relative_path: Some(rel.to_string()),
            thumbnail_path: None,
            mime_type: None,
            file_size: None,
            duration_seconds: Some(12),
            title: None,
            width: None,
            height: None,
            spoiler: false,
            ttl_seconds: None,
            skip_reason: None,
            extra_json: "{}".to_string(),
        }
    }

    fn rows_with(attachments: Vec<AttachmentRow>) -> ExportRows {
        ExportRows {
            chat_title: "t".to_string(),
            timeline_items: Vec::new(),
            messages: Vec::new(),
            service_events: Vec::new(),
            attachments,
            polls: Vec::new(),
            poll_options: Vec::new(),
        }
    }

    /// `argv_for` rendered back to UTF-8 strings, for concise assertions.
    fn argv_for_str(cmd: &TranscribeCommand, path: &str) -> Vec<String> {
        cmd.argv_for(std::ffi::OsStr::new(path))
            .iter()
            .map(|arg| arg.to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn parse_splits_and_finds_placeholder() {
        let cmd = TranscribeCommand::parse("whisper --model base {}").unwrap();
        assert_eq!(
            argv_for_str(&cmd, "/a/b.ogg"),
            ["whisper", "--model", "base", "/a/b.ogg"]
        );
    }

    #[test]
    fn parse_appends_path_when_no_placeholder() {
        let cmd = TranscribeCommand::parse("transcribe.sh").unwrap();
        assert_eq!(
            argv_for_str(&cmd, "/a/b.ogg"),
            ["transcribe.sh", "/a/b.ogg"]
        );
    }

    #[test]
    fn placeholder_matches_whole_element_only_and_repeats() {
        // Every exact `{}` is replaced; a substring like `--out={}` is not.
        let cmd = TranscribeCommand::parse("cp {} {}").unwrap();
        assert_eq!(argv_for_str(&cmd, "/p"), ["cp", "/p", "/p"]);
        let substr = TranscribeCommand::parse("x --out={}").unwrap();
        assert_eq!(argv_for_str(&substr, "/p"), ["x", "--out={}", "/p"]);
    }

    #[test]
    fn parse_rejects_unbalanced_quotes_and_empty() {
        assert!(TranscribeCommand::parse("cat '{}").is_err());
        assert!(TranscribeCommand::parse("   ").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn transcribes_in_scope_files_and_counts_failures() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("voice")).unwrap();
        std::fs::write(dir.path().join("voice/a.ogg"), "push it to Friday\n").unwrap();

        let export = rows_with(vec![
            voice_attachment("voice", "voice/a.ogg"), // transcribed
            voice_attachment("photo", "voice/a.ogg"), // out of scope → ignored
            voice_attachment("voice_message", "voice/missing.ogg"), // missing → failed
        ]);
        let cmd = TranscribeCommand::parse("cat {}").unwrap();
        let result = transcribe_attachments(&export, dir.path(), &cmd);

        assert_eq!(result.transcribed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(
            result.transcripts.get("voice/a.ogg").map(String::as_str),
            Some("push it to Friday"),
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_export_dir_is_skipped_not_transcribed() {
        use std::os::unix::fs::symlink;

        let export_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(export_dir.path().join("voice")).unwrap();

        // A secret file OUTSIDE the export directory.
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "TOP SECRET").unwrap();

        // The referenced media file is a symlink resolving outside the export dir,
        // the way a crafted export could try to exfiltrate an arbitrary file.
        symlink(&secret, export_dir.path().join("voice/a.ogg")).unwrap();

        let export = rows_with(vec![voice_attachment("voice", "voice/a.ogg")]);
        let cmd = TranscribeCommand::parse("cat {}").unwrap();
        let result = transcribe_attachments(&export, export_dir.path(), &cmd);

        assert_eq!(
            result.transcribed, 0,
            "symlink escape must not be transcribed"
        );
        assert_eq!(result.failed, 1, "symlink escape counts as a failure");
        assert!(
            result.transcripts.is_empty()
                && !result
                    .transcripts
                    .values()
                    .any(|t| t.contains("TOP SECRET")),
            "the secret must never enter the transcript map"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_escaping_export_dir_is_skipped_not_transcribed() {
        let export_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(export_dir.path().join("voice")).unwrap();

        // A secret file OUTSIDE the export directory.
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "TOP SECRET").unwrap();

        // A hard link INSIDE the export. Unlike a symlink, a hard link has no
        // separate target path: its canonical path stays inside the export, so
        // canonicalize + `starts_with` cannot detect it — yet its content is the
        // outside secret. The link count (> 1) is the signal that catches it.
        std::fs::hard_link(&secret, export_dir.path().join("voice/a.ogg")).unwrap();

        let export = rows_with(vec![voice_attachment("voice", "voice/a.ogg")]);
        let cmd = TranscribeCommand::parse("cat {}").unwrap();
        let result = transcribe_attachments(&export, export_dir.path(), &cmd);

        assert_eq!(
            result.transcribed, 0,
            "hard-link escape must not be transcribed"
        );
        assert_eq!(result.failed, 1, "hard-link escape counts as a failure");
        assert!(
            result.transcripts.is_empty()
                && !result.transcripts.values().any(|t| t.contains("SECRET")),
            "the secret must never enter the transcript map"
        );
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_relative_paths_invoke_the_engine_once() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("voice")).unwrap();
        std::fs::write(dir.path().join("voice/a.ogg"), "hello Friday").unwrap();

        // An engine that records each invocation by appending to a file, so the
        // test can prove dedup by counting calls rather than trusting the count.
        let counter = dir.path().join("calls");
        let engine = dir.path().join("engine.sh");
        std::fs::write(
            &engine,
            format!(
                "#!/bin/sh\nprintf x >> '{}'\ncat \"$1\"\n",
                counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Three attachment rows referencing the SAME media file (e.g. a re-merged
        // DB, or one voice note quoted several times).
        let export = rows_with(vec![
            voice_attachment("voice", "voice/a.ogg"),
            voice_attachment("voice_message", "voice/a.ogg"),
            voice_attachment("voice", "voice/a.ogg"),
        ]);
        let cmd = TranscribeCommand::parse(&format!("{} {{}}", engine.display())).unwrap();
        let result = transcribe_attachments(&export, dir.path(), &cmd);

        let calls = std::fs::read_to_string(&counter).unwrap_or_default();
        assert_eq!(
            calls.len(),
            1,
            "engine must run once per unique file, ran {} times",
            calls.len()
        );
        assert_eq!(result.transcribed, 1, "one unique file transcribed");
        assert_eq!(
            result.transcripts.get("voice/a.ogg").map(String::as_str),
            Some("hello Friday"),
        );
    }

    #[test]
    fn unsafe_relative_path_is_counted_as_failed() {
        // `safe_media_path` rejects the traversal path before any file access.
        let export = rows_with(vec![voice_attachment("voice", "../escape.ogg")]);
        let cmd = TranscribeCommand::parse("cat {}").unwrap();
        let result = transcribe_attachments(&export, Path::new("."), &cmd);
        assert_eq!(result.transcribed, 0);
        assert_eq!(result.failed, 1);
        assert!(result.transcripts.is_empty());
    }

    #[test]
    fn skip_reason_attachments_are_skipped_uncounted() {
        let mut att = voice_attachment("voice", "voice/a.ogg");
        att.skip_reason = Some("unsupported_media_shape".to_string());
        let export = rows_with(vec![att]);
        let cmd = TranscribeCommand::parse("cat {}").unwrap();
        let result = transcribe_attachments(&export, Path::new("."), &cmd);
        // A row with a skip_reason is a non-candidate: neither transcribed nor
        // counted as failed (matches spec §4).
        assert_eq!(result.transcribed, 0);
        assert_eq!(result.failed, 0);
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_and_empty_stdout_both_count_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("voice")).unwrap();
        std::fs::write(dir.path().join("voice/a.ogg"), "ignored").unwrap();
        let export = rows_with(vec![voice_attachment("voice", "voice/a.ogg")]);

        // `false` exits non-zero → failed.
        let nonzero = transcribe_attachments(
            &export,
            dir.path(),
            &TranscribeCommand::parse("false").unwrap(),
        );
        assert_eq!(nonzero.transcribed, 0);
        assert_eq!(nonzero.failed, 1);

        // `true` exits 0 but prints nothing → empty transcript → failed (bare placeholder).
        let empty = transcribe_attachments(
            &export,
            dir.path(),
            &TranscribeCommand::parse("true").unwrap(),
        );
        assert_eq!(empty.transcribed, 0);
        assert_eq!(empty.failed, 1);
        assert!(empty.transcripts.is_empty());
    }
}
