# telegram-export-sqlite

`telegram-export-sqlite` converts Telegram Desktop chat exports into SQLite,
can merge multiple exported chunks of the same conversation, and can render an
imported database back to Telegram Desktop-style HTML.

The tool is designed for archive work. It makes the chat timeline queryable
while preserving exporter-specific details that may be needed to reconstruct
the chat/event history later.

## Supported Formats

Input:

- Telegram Desktop HTML exports containing `messages.html`, `messages2.html`,
  and later split files.
- Telegram Desktop JSON exports containing `result.json`.

Output:

- SQLite databases with normalized tables plus preserved source details.
- A combined Telegram Desktop-style HTML export with one `messages.html` file
  and local support assets.

## Install

Build from source with Rust:

```bash
cargo build --release
```

The compiled binary is:

```bash
target/release/telegram-export-sqlite
```

You can also run the tool directly through Cargo:

```bash
cargo run -- --help
```

## Quick Start

Import an export directory into SQLite. A single path writes the database in
place as `<EXPORT_DIR>/chat.sqlite`:

```bash
telegram-export-sqlite import <EXPORT_DIR> --fts
```

Merge multiple SQLite chunks into one database:

```bash
telegram-export-sqlite merge combined.sqlite part1.sqlite part2.sqlite --fts
```

Export a database back to HTML:

```bash
telegram-export-sqlite export-html chat.sqlite chat-html --force
```

## Commands

### `import`

```bash
telegram-export-sqlite import <EXPORT_DIR> [DEST] [--force] [--incremental] [--fts]
```

`<EXPORT_DIR>` is a Telegram Desktop export directory. If `result.json` exists,
the importer treats the directory as a JSON export. Otherwise it imports
Telegram HTML message files in natural split-file order.

`import` has two output modes, chosen by whether `DEST` is given:

- **In place** (one positional): writes `<EXPORT_DIR>/chat.sqlite`. Media files
  are left where the export put them. If import fails, the previous
  `chat.sqlite` is preserved.
- **Bundle** (two positionals): builds a portable `<DEST>/chat.sqlite`, plus a
  `<DEST>/assets/` directory holding any referenced media (a text-only export
  produces just `chat.sqlite`). When present, `assets/` makes the bundle
  self-contained and movable or archivable as a unit. `DEST` is built through a
  sibling temporary directory and swapped in atomically only once the import
  succeeds; an existing `DEST` requires `--force` to replace, and `DEST` may
  not overlap `<EXPORT_DIR>`.

Options:

- `--force`: replace an existing output database (in-place mode) or `DEST`
  directory (bundle mode). If import fails, the previous database/bundle is
  preserved.
- `--incremental`: require an existing database and skip unchanged source files
  that were imported successfully before.
- `--fts`: create the optional `timeline_items_fts` full-text search table.

Examples:

```bash
telegram-export-sqlite import ./telegram-export
telegram-export-sqlite import ./telegram-export --fts
telegram-export-sqlite import ./telegram-export --incremental
telegram-export-sqlite import ./telegram-export --force
telegram-export-sqlite import ./telegram-export ./telegram-archive
telegram-export-sqlite import ./telegram-export ./telegram-archive --force
```

### `merge`

```bash
telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>... [--force] [--fts]
```

`merge` treats the input databases as continuation chunks of one logical
conversation. Inputs are processed in command-line order. Exact semantic
duplicates are skipped, and same-message-id conflicts with different content are
kept with warnings instead of being dropped.

Options:

- `--force`: replace an existing output database.
- `--fts`: create the optional full-text search table on the merged output.

Example:

```bash
telegram-export-sqlite merge combined.sqlite chunk-001.sqlite chunk-002.sqlite --fts
```

The command refuses an output path that is also one of the input databases.

### `export-html`

```bash
telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR> [--force]
```

`<INPUT_DB>` must be a SQLite database produced by this tool with a supported
schema version.

The exporter writes:

```text
messages.html
css/style.css
js/script.js
images/...
```

Example:

```bash
telegram-export-sqlite export-html chat.sqlite ./chat-html --force
```

The output directory must not already exist unless `--force` is passed. Export
writes through a sibling temporary directory before replacing the final
directory. The command refuses to run when the input database is inside the
output directory, because replacing the output directory would delete the source
database.

## Database Contents

The database stores one ordered logical timeline across messages, service
events, unsupported rows, attachments, and polls.

Core tables:

- `imports`: import or merge runs and summary counts.
- `source_files`: imported `messages*.html` files or `result.json` source files.
- `chats`, `chat_aliases`: chat identity and titles seen across imports.
- `users`: display names used by messages and service events.
- `timeline_items`: ordered timeline rows.
- `messages`: message text, rich-text entities, replies, forwards, buttons,
  reactions, and sender inference.
- `service_events`: classified Telegram service events plus fallback display
  text.
- `attachments`: relative media/document paths and metadata.
- `polls`, `poll_options`: Telegram poll data.
- `group_memberships`: membership facts inferred from service events.
- `import_warnings`: recoverable parse, import, or merge issues.

Optional table:

- `timeline_items_fts`: full-text search index created with `--fts`.

## Query Examples

Recent messages:

```sql
SELECT ti.ordinal, ti.timestamp, u.display_name AS sender, m.plain_text
FROM messages m
JOIN timeline_items ti ON ti.id = m.timeline_item_id
LEFT JOIN users u ON u.id = m.sender_user_id
ORDER BY ti.ordinal DESC
LIMIT 20;
```

Warnings by type:

```sql
SELECT warning_code, count(*)
FROM import_warnings
GROUP BY warning_code
ORDER BY count(*) DESC;
```

Attachments:

```sql
SELECT ti.ordinal, a.attachment_kind, a.relative_path, a.file_size
FROM attachments a
JOIN timeline_items ti ON ti.id = a.timeline_item_id
ORDER BY ti.ordinal;
```

Full-text search, when imported or merged with `--fts`:

```sql
SELECT rowid
FROM timeline_items_fts
WHERE timeline_items_fts MATCH 'search term';
```

## Fidelity Model

The schema is normalized for querying, but preservation is handled through
`extra_json`. If Telegram exports meaningful chat-history data that is not yet
represented by typed columns, the importer keeps it in `extra_json` instead of
dropping it.

Important boundaries:

- Split HTML files are treated as one logical timeline. The exact original
  division into `messages.html`, `messages2.html`, and later files is not a
  reconstruction target.
- HTML date separators are presentation-only. They are skipped on import and
  regenerated on HTML export.
- Media files are referenced by relative path and metadata. Media blobs are not
  embedded in SQLite.
- Bundle-mode import (`import <EXPORT_DIR> <DEST>`) copies referenced media into
  `assets/` under `DEST` and rewrites attachment `relative_path`/
  `thumbnail_path` to `assets/…` so they resolve relative to `chat.sqlite`. The
  original export-relative paths are preserved under `extra_json["bundle"]`.
- JSON exports do not contain Telegram Desktop's original HTML wrappers, CSS, or
  asset metadata. HTML generated from JSON-imported databases is therefore
  best-effort Telegram-style HTML.
- HTML export attempts Telegram Desktop-compatible structure, class names,
  escaping, grouping, and support-file paths where the database has enough
  information. It is not a bit-for-bit clone of Telegram Desktop output.

## Warnings

Recoverable issues are written to `import_warnings` and included in command
summaries. Warnings are expected for some exports, especially when media files
are missing from the export directory or an unsupported Telegram shape is
preserved without typed parsing.

Useful warning query:

```sql
SELECT source_file_id, warning_code, message, context_json
FROM import_warnings
ORDER BY id;
```

## Privacy Notes

The SQLite database contains chat content and preserved source metadata. JSON
imports preserve raw exporter payloads in `extra_json["source_json"]` where
needed for fidelity. Treat generated databases as sensitive chat archives.

Media files are not embedded, but relative media paths and metadata may still be
stored in the database. Bundle-mode import (`import <EXPORT_DIR> <DEST>`) does
copy referenced media files into `<DEST>/assets/`, so a bundle directory is
itself a self-contained archive; treat it with the same care as the database.

## License

Apache-2.0. See `LICENSE`.
