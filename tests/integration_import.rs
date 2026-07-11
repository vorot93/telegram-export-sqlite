use assert_cmd::Command;
use predicates::str::contains;

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn staged_export(name: &str) -> tempfile::TempDir {
    let src = std::path::Path::new("tests/fixtures").join(name);
    let temp = tempfile::tempdir().unwrap();
    copy_dir_recursive(&src, temp.path());
    temp
}

#[test]
fn cli_exposes_import_command() {
    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();

    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("import"))
        .stdout(contains("HTML or JSON"))
        .stdout(contains(
            "telegram-export-sqlite import <EXPORT_DIR> [DEST]",
        ));
}

#[test]
fn cli_import_help_uses_export_dir_value_name() {
    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();

    cmd.args(["import", "--help"])
        .assert()
        .success()
        .stdout(contains("EXPORT_DIR"))
        .stdout(contains("[DEST]"));
}

#[test]
fn cli_exposes_merge_command() {
    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();

    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("merge"))
        .stdout(contains(
            "telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>...",
        ));
}

#[test]
fn cli_exposes_export_html_command() {
    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();

    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("export-html"))
        .stdout(contains(
            "telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR>",
        ));
}

#[test]
fn cli_export_html_rejects_existing_export_directory_without_force() {
    let staged = staged_export("basic_export");
    let db = staged.path().join("chat.sqlite");
    let output_root = tempfile::tempdir().unwrap();
    let output = output_root.path().join("html");
    std::fs::create_dir(&output).unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            db.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("output directory already exists"));
}

#[test]
fn cli_export_html_exports_imported_sqlite_to_html() {
    let staged = staged_export("basic_export");
    let db = staged.path().join("chat.sqlite");
    let output_root = tempfile::tempdir().unwrap();
    let output = output_root.path().join("html");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            db.to_str().unwrap(),
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(contains("timeline items:"))
        .stdout(contains("messages:"))
        .stdout(contains("generated date separators:"));

    let html = std::fs::read_to_string(output.join("messages.html")).unwrap();
    assert!(html.contains("<title>Exported Data</title>"));
    assert!(html.contains("<div class=\"text bold\">Family Chat</div>"));
    assert!(html.contains("id=\"message101\""));
    assert!(html.contains("Hello <strong>family</strong>"));
    assert!(html.contains("href=\"photos/photo_1.jpg\""));
    assert!(output.join("css/style.css").is_file());
    assert!(output.join("js/script.js").is_file());
    assert!(output.join("images/media_file.png").is_file());
    assert!(output.join("images/media_file@2x.png").is_file());
}

#[test]
fn cli_export_html_exports_json_imported_sqlite_to_html() {
    let staged = staged_export("json_export");
    let db = staged.path().join("chat.sqlite");
    let output_root = tempfile::tempdir().unwrap();
    let output = output_root.path().join("html");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            db.to_str().unwrap(),
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(contains("messages: 3"))
        .stdout(contains("service events: 1"))
        .stdout(contains("polls: 1"));

    let html = std::fs::read_to_string(output.join("messages.html")).unwrap();
    assert!(html.contains("Family Chat"));
    assert!(html.contains("id=\"message101\""));
    assert!(html.contains("Hello <strong>family "));
    assert!(html.contains("href=\"files/report.pdf\""));
    assert!(html.contains("Lunch?"));
    assert!(html.contains("Alice invited Bob"));
}

#[test]
fn cli_export_html_force_replaces_stale_output_directory() {
    let staged = staged_export("basic_export");
    let db = staged.path().join("chat.sqlite");
    let output_root = tempfile::tempdir().unwrap();
    let output = output_root.path().join("html");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("stale.txt"), "old export").unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            db.to_str().unwrap(),
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();

    assert!(output.join("messages.html").is_file());
    assert!(!output.join("stale.txt").exists());
}

#[test]
fn cli_export_html_rejects_input_database_inside_output_directory() {
    let staged = staged_export("basic_export");
    let output = staged.path().to_path_buf();
    let db = output.join("chat.sqlite");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            db.to_str().unwrap(),
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .failure()
        .stderr(contains(
            "input database must not be inside output directory",
        ));

    assert!(db.is_file());
}

#[test]
fn exported_html_reimports_with_same_core_semantics() {
    let staged = staged_export("basic_export");
    let original_db = staged.path().join("chat.sqlite");
    let temp = tempfile::tempdir().unwrap();
    let exported_dir = temp.path().join("exported-html");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            original_db.to_str().unwrap(),
            exported_dir.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", exported_dir.to_str().unwrap(), "--force"])
        .assert()
        .success();

    let reimported_db = exported_dir.join("chat.sqlite");
    let original = rusqlite::Connection::open(original_db).unwrap();
    let reimported = rusqlite::Connection::open(reimported_db).unwrap();

    for table in ["messages", "service_events", "attachments", "polls"] {
        let original_count: i64 = original
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        let reimported_count: i64 = reimported
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(reimported_count, original_count, "table {table}");
    }

    let original_text: String = original
        .query_row(
            "SELECT plain_text FROM messages WHERE telegram_message_id = 101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let reimported_text: String = reimported
        .query_row(
            "SELECT plain_text FROM messages WHERE telegram_message_id = 101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reimported_text, original_text);

    let reimported_reply: i64 = reimported
        .query_row(
            "SELECT reply_to_message_id FROM messages WHERE telegram_message_id = 102",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reimported_reply, 101);

    let reimported_media_path: String = reimported
        .query_row(
            "SELECT relative_path FROM attachments WHERE relative_path = 'photos/photo_1.jpg'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reimported_media_path, "photos/photo_1.jpg");
}

#[test]
fn json_fallback_service_events_survive_html_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let json_dir = temp.path().join("json-export");
    let exported_dir = temp.path().join("exported-html");
    std::fs::create_dir(&json_dir).unwrap();
    std::fs::write(
        json_dir.join("result.json"),
        r#"{
  "about": "Telegram Desktop export",
  "chats": {
    "about": "Exported chats",
    "list": [
      {
        "name": "Fallback Services",
        "type": "private_group",
        "id": 42,
        "messages": [
          {
            "id": 1,
            "type": "service",
            "date": "2025-02-12T09:00:00",
            "date_unixtime": "1739350800",
            "action": "edit_group_title",
            "title": "Fallback Services"
          },
          {
            "id": 2,
            "type": "service",
            "date": "2025-02-12T10:00:00",
            "date_unixtime": "1739354400",
            "actor": "Fallback Services",
            "actor_id": "channel42",
            "action": "migrate_from_group"
          },
          {
            "id": 3,
            "type": "service",
            "date": "2025-02-12T11:00:00",
            "date_unixtime": "1739358000",
            "action": "pin_message",
            "message_id": 2
          }
        ]
      }
    ]
  }
}
"#,
    )
    .unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", json_dir.to_str().unwrap(), "--force"])
        .assert()
        .success()
        .stdout(contains("service events: 3"));

    let original_db = json_dir.join("chat.sqlite");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "export-html",
            original_db.to_str().unwrap(),
            exported_dir.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(contains("service events: 3"));

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", exported_dir.to_str().unwrap(), "--force"])
        .assert()
        .success()
        .stdout(contains("service events: 3"));

    let reimported_db = exported_dir.join("chat.sqlite");
    let conn = rusqlite::Connection::open(reimported_db).unwrap();
    let event_types = {
        let mut stmt = conn
            .prepare(
                "SELECT se.event_type
                 FROM service_events se
                 JOIN timeline_items ti ON ti.id = se.timeline_item_id
                 ORDER BY ti.ordinal",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        event_types,
        vec!["edit_group_title", "migrate_from_group", "pin_message"]
    );

    let unknown_service_warnings: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM import_warnings WHERE warning_code = 'unknown_service_event'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown_service_warnings, 0);
}

#[test]
fn cli_imports_fixture_to_sqlite() {
    let staged = staged_export("basic_export");

    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();
    cmd.args([
        "import",
        staged.path().to_str().unwrap(),
        "--force",
        "--fts",
    ])
    .assert()
    .success()
    .stdout(contains("files imported: 2"))
    .stdout(contains("warnings:"));

    let db = staged.path().join("chat.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    let messages: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert!(messages >= 5);
}

#[test]
fn cli_imports_json_fixture_to_sqlite() {
    let staged = staged_export("json_export");

    let mut cmd = Command::cargo_bin("telegram-export-sqlite").unwrap();
    cmd.args([
        "import",
        staged.path().to_str().unwrap(),
        "--force",
        "--fts",
    ])
    .assert()
    .success()
    .stdout(contains("files imported: 1"))
    .stdout(contains("timeline items: 4"))
    .stdout(contains("messages: 3"))
    .stdout(contains("service events: 1"))
    .stdout(contains("attachments: 1"))
    .stdout(contains("warnings: 0"));

    let db = staged.path().join("chat.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    let source_path: String = conn
        .query_row("SELECT relative_path FROM source_files", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source_path, "result.json");
    let polls: i64 = conn
        .query_row("SELECT count(*) FROM polls", [], |row| row.get(0))
        .unwrap();
    assert_eq!(polls, 1);
    let fts_matches: i64 = conn
        .query_row(
            "SELECT count(*) FROM timeline_items_fts WHERE timeline_items_fts MATCH 'family'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fts_matches, 1);
}

#[test]
fn cli_merges_imported_sqlite_databases() {
    let first_staged = staged_export("basic_export");
    let second_staged = staged_export("json_export");
    let temp = tempfile::tempdir().unwrap();
    let merged = temp.path().join("merged.db");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", first_staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();
    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", second_staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    let first = first_staged.path().join("chat.sqlite");
    let second = second_staged.path().join("chat.sqlite");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "merge",
            merged.to_str().unwrap(),
            first.to_str().unwrap(),
            second.to_str().unwrap(),
            "--force",
            "--fts",
        ])
        .assert()
        .success()
        .stdout(contains("input databases: 2"))
        .stdout(contains("timeline items:"));

    let conn = rusqlite::Connection::open(merged).unwrap();
    let timeline_items: i64 = conn
        .query_row("SELECT COUNT(*) FROM timeline_items", [], |row| row.get(0))
        .unwrap();
    assert!(timeline_items > 8);
    let fts_matches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM timeline_items_fts WHERE timeline_items_fts MATCH 'family'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(fts_matches > 0);
}

#[test]
fn incremental_import_skips_unchanged_files() {
    let staged = staged_export("basic_export");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--incremental"])
        .assert()
        .success()
        .stdout(contains("files skipped: 2"));
}

#[test]
fn incremental_json_import_skips_unchanged_result() {
    let staged = staged_export("json_export");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--force"])
        .assert()
        .success();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args(["import", staged.path().to_str().unwrap(), "--incremental"])
        .assert()
        .success()
        .stdout(contains("files seen: 1"))
        .stdout(contains("files imported: 0"))
        .stdout(contains("files skipped: 1"));
}

#[test]
fn cli_import_builds_portable_bundle() {
    let staged = staged_export("basic_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("attachments:"));

    assert!(dest.join("chat.sqlite").is_file());
    // basic_export's messages.html lives under chat_001/, so attachment paths
    // are stored export-root-relative as chat_001/... and bundle under that
    // same prefix (see bundle.rs's relocate_media tests for the same fixture).
    assert!(dest.join("assets/chat_001/photos/photo_1.jpg").is_file());
    assert!(dest.join("assets/chat_001/files/report.pdf").is_file());
}

#[test]
fn cli_import_bundle_refuses_dest_inside_export() {
    let staged = staged_export("basic_export");
    let dest = staged.path().join("inside");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("must not overlap"));
}

#[test]
fn cli_import_bundle_refuses_existing_dest_without_force() {
    let staged = staged_export("basic_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");
    std::fs::create_dir(&dest).unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("output directory already exists"));
}

#[test]
fn cli_import_bundle_replaces_existing_dest_with_force() {
    let staged = staged_export("basic_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("stale.txt"), "old").unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();

    assert!(dest.join("chat.sqlite").is_file());
    assert!(!dest.join("stale.txt").exists()); // whole dir replaced, not merged
}

#[test]
fn cli_import_bundles_json_export_and_copies_media() {
    // The json_export fixture's result.json sits at the export root (unlike
    // basic_export's chat_001/-nested messages.html), so its "files/report.pdf"
    // reference is already export-root-relative and bundles without a chat
    // subdirectory prefix. Confirmed via `find <DEST>/assets` against a real run.
    let staged = staged_export("json_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("json-archive");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(dest.join("chat.sqlite").is_file());
    assert!(dest.join("assets/files/report.pdf").is_file());
}

#[test]
fn cli_import_bundle_incremental_rebuilds_and_keeps_assets() {
    let staged = staged_export("basic_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Bundle incremental always full-rebuilds into a fresh temp bundle (it
    // only consults DEST/chat.sqlite for the finished-source-files guard), so
    // this should succeed and reproduce the same bundle contents.
    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
            "--incremental",
        ])
        .assert()
        .success();

    assert!(dest.join("chat.sqlite").is_file());
    assert!(dest.join("assets/chat_001/photos/photo_1.jpg").is_file());
    assert!(dest.join("assets/chat_001/files/report.pdf").is_file());
}

#[test]
fn cli_import_bundle_fails_when_dest_is_a_file() {
    let staged = staged_export("basic_export");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");
    std::fs::write(&dest, "not a directory").unwrap();

    Command::cargo_bin("telegram-export-sqlite")
        .unwrap()
        .args([
            "import",
            staged.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("output path is a file"));
}
