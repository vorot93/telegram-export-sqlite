# AGENTS.md

Guidance for future AI coding sessions in this repository.

## Project Mission

`telegram-export-sqlite` is an archive-fidelity CLI for Telegram Desktop
exports:

1. Import Telegram HTML or JSON chat exports to SQLite.
2. Preserve enough detail to reconstruct the chat/event history later.
3. Merge multiple SQLite exports as continuation chunks.
4. Export SQLite back to Telegram Desktop-style HTML.

Do not optimize for a tidy normalized schema at the cost of losing Telegram
export detail. Normalized tables are for querying; `extra_json` is the
preservation boundary.

## Non-Negotiable Fidelity Rules

- No meaningful chat/event-history data should be dropped. If it does not fit a
  typed column yet, preserve it in `extra_json`.
- Preserve raw JSON exporter payloads under `extra_json["source_json"]` when
  importing JSON.
- Treat split HTML files as one logical timeline. Exact original division into
  `messages.html`, `messages2.html`, etc. is not important.
- Date separators are presentation-only. Do not store or merge them as semantic
  service events; regenerate them on HTML export.
- Media is path-only. Store relative paths and metadata; do not embed blobs.
- Unknown service/message/media shapes should become preserved rows plus
  warnings, not silent loss.
- Telegram Desktop source is the best prior art for export fidelity. Prefer it
  over third-party reimplementations when behavior is unclear.

## Code Map

- `src/cli.rs`: CLI surface and printed summaries.
- `src/discovery.rs`: export format discovery. `result.json` detection wins
  before HTML fallback.
- `src/importer.rs`: import orchestration, force/incremental safety, FTS, and
  choosing in-place vs. bundle output.
- `src/bundle.rs`: post-import bundling pass — copies referenced media to
  `assets/` (created only when the archive references media; a text-only
  export produces no `assets/`), rewrites typed attachment paths, preserves
  originals in `extra_json["bundle"]`. Idempotent. Canonicalizes both the
  export root and each resolved media path before copying, so a symlink
  inside the export cannot smuggle a file from outside the export directory
  into the bundle (see `CopyOutcome::Escapes`).
- `src/parser.rs`: Telegram Desktop HTML parser.
- `src/json_parser.rs`: Telegram Desktop JSON parser.
- `src/service.rs`: service-event classification from visible text.
- `src/text.rs`: HTML rich-text extraction and entity preservation.
- `src/db.rs`: schema, inserts, FTS creation. Current `SCHEMA_VERSION` is `1`.
- `src/merge.rs`: SQLite continuation merge, dedupe/conflict behavior.
- `src/html_export.rs`: SQLite to HTML command orchestration.
- `src/html_export/rows.rs`: export row loaders and schema validation.
- `src/html_export/render.rs`: Telegram Desktop-style HTML rendering.
- `src/html_export/assets.rs`: local non-verbatim support assets.
- `src/media_path.rs`: safe relative media-path/href-scheme validation, shared
  by HTML export and bundle media copying.
- `src/output_dir.rs`: atomic output-directory replacement via a sibling temp
  dir and backup swap, shared by HTML export and bundle import.
- `tests/integration_import.rs`: CLI and round-trip integration tests.
- `tests/fixtures/`: small HTML and JSON export fixtures.

## CLI Contracts

Keep these commands and help text in sync when changing behavior:

```bash
telegram-export-sqlite import <EXPORT_DIR> [DEST] [--force] [--incremental] [--fts]
telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>... [--force] [--fts]
telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR> [--force]
```

Safety expectations:

- In-place import (`import <EXPORT_DIR>`) writes `<EXPORT_DIR>/chat.sqlite`; the
  previous database is preserved if import fails.
- Bundle import (`import <EXPORT_DIR> <DEST>`) builds through a sibling temp
  directory and atomically replaces `DEST`; it refuses a `DEST` that overlaps
  the export dir and requires `--force` to replace an existing `DEST`.
- Incremental import requires an existing DB and skips unchanged source files.
- Merge writes through a temp DB and refuses output equal to any input.
- Export HTML writes through a sibling temp dir and refuses an input DB located
  inside the output directory.

## Schema Rules

- Every schema change needs tests and an explicit decision about
  `SCHEMA_VERSION` / `PRAGMA user_version`.
- Required tables are validated by merge and HTML export loaders. Update those
  lists with schema changes.
- Do not replace `extra_json` wholesale when augmenting rows. Merge new
  provenance or parsed fields into the existing object.
- FTS is optional. Preserve behavior for databases without
  `timeline_items_fts`.

## Import Notes

- HTML parser state matters across split files. Joined messages may infer sender
  context from prior messages.
- JSON import does not have Telegram Desktop HTML wrapper details; preserve raw
  payloads and render best-effort HTML later.
- Missing media files should produce warnings while preserving attachment rows
  and relative paths.
- Service fallback display text from JSON can be intentionally non-canonical.
  Keep `src/service.rs` able to reclassify fallback service labels emitted by
  this tool's HTML exporter.
- Bundle media resolves as `dirname(chat.sqlite) + relative_path`: bundle mode
  prefixes typed attachment paths with `assets/` and preserves the original
  export-relative paths under `extra_json["bundle"]` (merged into the existing
  object, never clobbering `source_json` or other keys). No schema change.
  `relocate_media` skips paths already under `assets/`, so re-running it
  (incremental re-sync, retries) never double-prefixes.
- Bundle-mode database builds happen in a temp sibling directory that only
  becomes `DEST` at the very end (`replace_output_dir`). `imports.output_path`
  must record the final `DEST/chat.sqlite` path (threaded through
  `build_database`/`run_full_rebuild_safely` as `existing_db`/
  `recorded_output_path`), not the ephemeral temp write target (`output_db`),
  or provenance points at a directory that no longer exists after the swap.
- Bundle `--incremental` always full-rebuilds: the temp write target never
  equals the existing `DEST/chat.sqlite`, so the "all source files finished"
  no-op fast path is in-place-only. It still reads `DEST/chat.sqlite` for the
  missing-previously-imported-source guard.
- Only `import` uses the in-place/bundle output model. `merge` and `export-html`
  still take or produce a bare, caller-named database path (their pre-bundle
  contract); bringing them onto the bundle model is future work, and
  `export-html` does not yet copy chat media into its own HTML output.

## Merge Notes

- Inputs are continuation chunks in command-line order.
- Exact semantic duplicates are skipped; first occurrence wins.
- Same Telegram message id with different semantics is a conflict to keep, not
  a row to drop.
- Merge provenance belongs in `extra_json["merge_source"]`.
- Duplicate/conflict events should be visible in `import_warnings`.

## HTML Export Notes

- Output target is one combined `messages.html`.
- Attempt Telegram Desktop-like structure, escaping, class names, grouping, and
  support-file paths where SQLite has enough data.
- Do not vendor Telegram Desktop GPL assets into this Apache-2.0 project without
  an explicit license decision. `assets.rs` intentionally writes local
  non-verbatim placeholders.
- Media hrefs must remain relative and safe. Reject or avoid absolute paths,
  schemes, backslashes, raw `..`, and percent-encoded traversal.
- Preserve service-event row/type round trips even when exact Telegram wording
  cannot be reconstructed from missing JSON actor/title/member data.

## Documentation Discipline

`README.md` is for end users. Keep implementation details, verification
workflow, and AI-session guidance in this file instead.

When changing format support or fidelity boundaries, update all relevant
surfaces together:

1. Parser/discovery/export behavior.
2. CLI help and end-user README wording.
3. Focused tests and, when relevant, round-trip tests.
4. This file, when future agents need new constraints or warnings.

Do not add private archive names, absolute personal paths, one-off local smoke
counts, or development-only corpus details to `README.md` or `AGENTS.md`.

## Testing Notes

- In-place import writes `chat.sqlite` INTO the export directory, and
  `tests/fixtures/**` is committed and must stay read-only. Any test that
  imports must first stage a writable copy of the fixture into a tempdir — use
  the `pub(crate)` `staged_export`/`copy_dir_recursive` helpers in
  `importer.rs`'s test module (re-implemented in `tests/integration_import.rs`,
  a separate crate that cannot see crate-internal test code). A clean run leaves
  `git status --porcelain tests/fixtures` empty; if it does not, a test imported
  a fixture in place.
- Bundled asset paths depend on each fixture's layout and are easy to assert
  wrong. `basic_export` nests `messages.html` under `chat_001/`, so its
  attachment paths are stored as `chat_001/…` and bundle media lands at
  `assets/chat_001/photos/photo_1.jpg` and `assets/chat_001/files/report.pdf`
  (the HTML parser joins each href with its message file's parent directory).
  `json_export` keeps `result.json` at its root, so its media is
  `assets/files/report.pdf`. Import once and inspect `<DEST>/assets` rather than
  assuming a flat `assets/photos/…` layout.
- Symlink-escape behavior in bundling is unix-only in tests (`#[cfg(unix)]`,
  built with `std::os::unix::fs::symlink`); the guard itself is
  platform-agnostic.

## Required Verification

Run before claiming completion:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Add focused tests for changed behavior. Common filters:

```bash
cargo test --test integration_import
cargo test html_export::render
cargo test service::tests
cargo test merge::tests
```

For fidelity-sensitive changes, also run a smoke test with a representative
Telegram Desktop export when one is available. Use bundle mode (two
positionals) so the source export directory is left untouched:

```bash
cargo run -- import <EXPORT_DIR> <SMOKE_DIR> --force
cargo run -- export-html <SMOKE_DIR>/chat.sqlite <SMOKE_HTML_DIR> --force
cargo run -- import <SMOKE_HTML_DIR> <SMOKE_REIMPORT_DIR> --force
```

Compare timeline, message, service-event, attachment, warning, and generated
date-separator counts between `<SMOKE_DIR>/chat.sqlite` and
`<SMOKE_REIMPORT_DIR>/chat.sqlite` before and after the round trip. If counts
change, explain why with evidence.

## Working Style

- Avoid broad refactors while fixing fidelity issues.
- Keep changes small and auditable against the no-data-loss requirement.
- Preserve existing public CLI behavior unless the user explicitly approves a
  contract change.
- Prefer focused fixtures and tests that demonstrate the exact Telegram shape or
  failure mode being handled.
- If behavior is unclear, inspect Telegram Desktop's exporter behavior before
  inventing a local convention.
