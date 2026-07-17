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
  over third-party reimplementations when behavior is unclear. HTML parser class
  names, status strings, and service phrasings must match real `tdesktop`
  `export_output_html.cpp` output; verify against it rather than inventing markup,
  and cover it with a real-markup fixture (see `tests/fixtures/tdesktop_media`).
  The repo is **`github.com/telegramdesktop/tdesktop`, default branch `dev`** (NOT
  `telegram/tdesktop`, which 404s); the **tdesktop Ground Truth** section below
  lists the specific cpp files and the ground truth extracted from them.

## tdesktop Ground Truth

The single highest-leverage rule: the HTML parser, service classifier, and
discovery must match **real Telegram Desktop output**, not hand-written fixtures.
The original fixtures used invented class names (`voice_message` instead of the
real `media_voice_message`) and status formats, so voice/size/duration/service
parsing silently broke on genuine exports. Never guess a tdesktop layout or a
JSON export shape — read the cpp below or a real export, and cover the shape with
a real-markup fixture (`tests/fixtures/tdesktop_media`).

Authoritative source: **`github.com/telegramdesktop/tdesktop`, branch `dev`**.

- `export/output/export_output_html.cpp` — media anchor classes
  (`media clearfix pull_left block_link <classes>`), `.status` strings,
  service-message phrasings, date dividers (negative ids via `--_dateMessageId`),
  and `HtmlWriter::messagesFile` = `"messages" + (index>0 ? number(index+1) : "")
  + ".html"` → `messages.html`, `messages2.html`, `messages3.html`, … (never
  `messages1.html`/`messages0.html`; 1000 messages per file). Media hrefs/src come
  from `HtmlWriter::Wrap::relativePath(path)` = `_base + path` — a plain relative
  path, **never percent-encoded** — so export-html's C46 percent-encoding is a
  safe superset that resolves to the same file, not a fidelity break.
- `export/output/export_output_json.cpp` — JSON export shapes. `pushFrom(label)`
  emits `<label>` and `<label>_id` together under one `if (message.fromId)` guard,
  so `from`/`from_id` and (service) `actor`/`actor_id` are **both-or-neither**;
  `wrapPeerName` = `StringAllowNull(peer.name())` writes a bare JSON `null` when the
  peer name is empty (a deleted/unresolved account), and `pushUserNames` does the
  same per `members` entry — which is why a null `from`/`actor`/member with an id
  present is labeled `DELETED_ACCOUNT_NAME` (that literal is HTML-only; JSON emits
  `null`). A single-chat export has a top-level `messages`; a full account has
  top-level `chats`/`left_chats` (the multi-chat discriminator).
- `export/data/export_data_types.cpp` — `FormatFileSize`, `FormatDuration`, and
  `DocumentFolder` → the attachment subdirectory names (`files`, `video_files`,
  `voice_messages`, `round_video_messages`, `animations`, `stickers`; plus
  `photos`, `profile_pictures`, `stories`). Split message files are direct children
  of a chat directory (`chat_N`/`chats`), never inside these media subdirs — the
  basis for the discovery guard (`is_message_split_file`, `MEDIA_SUBDIRS`).
- `ui/text/format_values.cpp` — `FormatSizeText` (one-decimal KB/MB),
  `FormatDurationText` (`H:MM:SS`, zero-padded minutes).

## Code Map

- `src/cli.rs`: CLI surface and printed summaries.
- `src/discovery.rs`: export format discovery. `result.json` detection wins
  before HTML fallback. Split-message discovery is constrained to the real
  tdesktop layout (verified against `export_output_html.cpp`
  `HtmlWriter::messagesFile` and `export_data_types.cpp` `DocumentFolder`):
  genuine files are named exactly `messages.html` / `messages{N≥2}.html` and sit
  at a chat-directory root; a `messages*.html` inside a media subdirectory
  (`files/`, `photos/`, `video_files/`, …) is a chat-supplied document
  masquerading as a split page and is ignored (`MEDIA_SUBDIRS`,
  `is_message_split_file`). Chat dirs are `chat_N`/`chats`, never media names, so
  the filter never drops a real split file.
- `src/importer.rs`: import orchestration, force/incremental safety, FTS, and
  choosing in-place vs. bundle output. Incremental refresh validates the existing
  DB's schema version *before* it is stamped (`validate_input_database`), refuses a
  chat-identity mismatch (`refuse_incremental_chat_mismatch`), preserves prior
  bundle `assets/` the new export dropped (`preserve_prior_assets`), and recovers a
  crash-stranded backup (`announce_or_recover_stray_backups`).
- `src/bundle.rs`: post-import bundling pass — copies referenced media to
  `assets/` (created only when the archive references media; a text-only
  export produces no `assets/`), rewrites typed attachment paths, preserves
  originals in `extra_json["bundle"]`. Idempotent. Canonicalizes both the
  export root and each resolved media path before copying, so neither a
  symlink nor a hard link (link count > 1) inside the export can smuggle a
  file from outside the export directory into the bundle (see
  `CopyOutcome::Escapes`). Two sources that differ only by case (`Report.pdf`
  vs `report.pdf`) are written to distinct names (`report_2.pdf`) so the bundle
  stays correct on a case-insensitive filesystem regardless of where it was
  built (`disambiguated_path`, `CopiedMedia`).
- `src/parser.rs`: Telegram Desktop HTML parser.
- `src/json_parser.rs`: Telegram Desktop JSON parser.
- `src/service.rs`: service-event classification from visible text.
- `src/text.rs`: HTML rich-text extraction and entity preservation.
- `src/db.rs`: schema, inserts, FTS creation. Current `SCHEMA_VERSION` is `1`
  (stamped into `PRAGMA user_version`). Imports always build into a fresh temp
  DB, so there is no in-place migration path; a schema change updates
  `create_schema` and bumps the constant. The version gate
  (`export_rows::validate_input_database`) is an **exact match**, so merge and
  export refuse any DB whose version is not the current one — such a database
  must be re-imported before it can be merged or exported. The `insert_*`
  SELECT-then-UPDATE "upsert" branches and the `insert_source_file` OR-lookup are
  dead on every current path (imports build into a fresh temp DB; discovery makes
  `(path, parse_order)` a stable bijection) — intentional dead code, not a
  simplification target.
- `src/merge.rs`: SQLite continuation merge, dedupe/conflict behavior.
- `src/html_export.rs`: SQLite to HTML command orchestration; copies each
  referenced media file into the output tree (`copy_media_into_output`) so links
  resolve.
- `src/export_rows.rs`: shared SQLite row loader + schema validation
  (`REQUIRED_TABLES`, `validate_input_database`, `load_export`, row structs),
  consumed by both HTML and LLM export. (Formerly `html_export/rows.rs`.)
- `src/html_export/render.rs`: Telegram Desktop-style HTML rendering.
- `src/html_export/assets.rs`: local non-verbatim support assets.
- `src/llm_export.rs`: `export-llm` orchestration — validate, load, render,
  write file or stdout, print stderr summary.
- `src/llm_export/render.rs`: compact-Markdown renderer for LLM ingestion.
- `src/reactions.rs`: shared reaction/emoji parsing helpers (`reaction_emoji`,
  `reaction_count`; `named_emoji` private) used by both renderers. Only the
  renderer-specific `render_reactions*` wrappers stay in the two `render.rs`
  files, because HTML and Markdown format reactions differently.
- `src/media_path.rs`: safe relative media-path/href-scheme validation and
  `encode_media_path` (percent-encodes a validated path for an href/src; C46),
  shared by HTML export and bundle media copying.
- `src/media_copy.rs`: `copy_guarded` — one guarded media-file copy (canonicalize
  inside the export root, refuse a symlink/hard-link escape, skip missing), shared
  by bundle creation and HTML export so both apply the same anti-smuggling policy.
- `src/output_dir.rs`: atomic output-directory replacement via a sibling temp
  dir and backup swap, shared by HTML export and bundle import.
  `find_stray_backup_dirs` enumerates the `.<name>.backup-*` siblings a crash
  between the two renames can leave behind, so bundle import can recover/announce
  them.
- `src/db_swap.rs`: atomic single-file database replacement via a temporary
  sibling file (`temporary_database_path`, `replace_database`,
  `cleanup_temp_database`), shared by `src/importer.rs` and `src/merge.rs`. The
  file-level analog of `output_dir.rs`'s directory-level swap.
- `tests/integration_import.rs`: CLI and round-trip integration tests.
- `tests/fixtures/`: small HTML and JSON export fixtures. `basic_export` is a
  broad smoke fixture; `tdesktop_media` deliberately mirrors real Telegram
  Desktop markup (the `media_voice_message`/`media_audio_file`/`video_file_wrap`
  anchor classes, `video_duration` overlay, and the combined `"M:SS, N.N MB"`
  status string) so the media/service parsers are exercised against ground truth,
  not invented class names.

## CLI Contracts

Keep these commands and help text in sync when changing behavior:

```bash
telegram-export-sqlite import <EXPORT_DIR> [DEST] [--force] [--incremental] [--fts]
telegram-export-sqlite merge <OUTPUT_DB> <INPUT_DB>... [--force] [--fts]
telegram-export-sqlite export-html <INPUT_DB> <OUTPUT_DIR> [--force]
telegram-export-sqlite export-llm <INPUT_DB> <OUTPUT_FILE> [--force]
```

Safety expectations:

- In-place import (`import <EXPORT_DIR>`) writes `<EXPORT_DIR>/chat.sqlite`; the
  previous database is preserved if import fails.
- Bundle import (`import <EXPORT_DIR> <DEST>`) builds through a sibling temp
  directory and atomically replaces `DEST`; it refuses a `DEST` that overlaps
  the export dir and requires `--force` to replace an existing `DEST`.
- Incremental import requires an existing DB and skips unchanged source files. It
  refuses a DB whose schema version is not current (validating before the schema
  is stamped, so a foreign/newer DB is never mutated) and refuses a refresh whose
  chat identity (chat-title set) differs from the archived chat (titles accumulate
  in `chat_aliases`, so a rename seen across prior imports still matches; a chat
  renamed to a never-before-seen title is a false positive — refused, recover with
  a fresh import). A bundle refresh
  preserves previously archived `assets/` media absent from the new export, and
  recovers a `.<name>.backup-*` bundle stranded by a crash in a prior atomic swap.
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
- `messages.telegram_message_id` is **nullable**: a message the source
  gives no id is stored as `NULL`/`None`, never a fabricated stand-in. The
  nullable `timeline_items.telegram_message_id` is the single authoritative id;
  the model type is `Option<i64>`. Reply targets always carry an id, so the HTML
  exporter emits no `id="message…"` attribute for an id-less message (its
  `parse_message_id` reads that attribute back, so a numeric stand-in would
  re-fabricate the id on round-trip).

## Import Notes

- HTML parser state matters across split files. Joined messages may infer sender
  context from prior messages.
- JSON import does not have Telegram Desktop HTML wrapper details; preserve raw
  payloads and render best-effort HTML later.
- JSON import is single-chat. `json_parser::export_dialogs` refuses a full-account
  export (more than one dialog across `chats.list`/`left_chats.list`); a
  single-chat export is exactly one dialog (root `messages`, or a one-element
  `chats.list`). Flattening multiple chats collided message ids across chats (C21)
  and mislabeled the result (C53). tdesktop discriminator (verified against
  `export_output_json.cpp`): a top-level `messages` key is one chat, a top-level
  `chats` key is a full account.
- A shared contact's `contact_vcard` is a real relative path under `contacts/`
  (default settings) or a `(File not included…)` placeholder; it is routed through
  `path_attachment` so a real path is existence-checked and bundle-copied and a
  placeholder degrades to a skip_reason (C24), with `contact_vcard_file_size` as
  the size. A contact with no vCard bytes stays pathless metadata.
- Service `members` may contain JSON `null` — a deleted/unresolved account, which
  tdesktop's JSON writer emits as bare `null` (never a "Deleted Account" string;
  that literal is HTML-only). `service_member_names` preserves a `null` as the
  shared `DELETED_ACCOUNT_NAME` label (the same one a null message `from` uses),
  keeping the member counted rather than dropping it (C25). A service event's
  `actor` takes the same path: tdesktop's `pushFrom("actor")` emits `actor` and
  `actor_id` together, so a null `actor` with a present `actor_id` is labeled
  `DELETED_ACCOUNT_NAME` too, not dropped to `None` (the raw null stays in
  `extra_json`).
- Missing media files should produce warnings while preserving attachment rows
  and relative paths.
- The checksum stored in `source_files` is the SHA-256 of the exact bytes the
  parser read (`ParsedExport::source_checksum`, `discovery::sha256_hex`), not the
  separate discovery-time hash. If a source file is rewritten between discovery
  and parse, the stored checksum still matches the imported content, so
  incremental never skips a file whose recorded checksum disagrees with what was
  imported. Discovery's hash still drives the pre-parse skip decision.
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
  contract); bringing them onto the bundle model is future work. `export-html`
  does copy referenced media into its output (see HTML Export Notes), reading it
  from beside `<INPUT_DB>`.

## Merge Notes

- Inputs are continuation chunks; the merged timeline is re-numbered
  chronologically by timestamp afterwards (`reorder_timeline_chronologically`),
  so correctness no longer depends on command-line order. Undated items inherit
  the preceding dated item's time and are otherwise stable.
- Fingerprints are chat-scoped and deliberately exclude exporter-variable fields:
  service-event timestamp/`display_text` differ between HTML and JSON for one
  event, and the attachment `relative_path` is layout-dependent (per-run
  filenames, `assets/` under bundling) — only attachment kinds identify media.
  The per-run `merge_source` stamp is stripped before hashing so re-merge is
  idempotent.
- Duplicates resolve newest-wins (`Richness`): a later edit wins, then more
  reactions, then more poll voters; the kept row's mutable content (message
  edit/reactions/text, poll tallies) is overwritten in place.
- Same Telegram message id with different semantics is a conflict to keep, not
  a row to drop. User identity is carried by `telegram_user_id`, not display
  name via `db::optional_user_id` (reused by merge).
- A merged database can be re-merged: `source_files` has no
  `UNIQUE(import_id, relative_path)` and merge re-numbers `parse_order`.
- Merge provenance belongs in `extra_json["merge_source"]`.
- Duplicate/conflict events should be visible in `import_warnings`.

## HTML Export Notes

- Output target is one combined `messages.html`.
- Attempt Telegram Desktop-like structure, escaping, class names, grouping, and
  support-file paths where SQLite has enough data.
- Do not vendor Telegram Desktop GPL assets into this Apache-2.0 project without
  an explicit license decision. `assets.rs` intentionally writes local
  non-verbatim placeholders.
- Media is referenced by the attachment's canonical `relative_path` (validated by
  `media_path::safe_media_path`, then percent-encoded via `encode_media_path`; the
  stale original href in `extra_json` is ignored — it is relative to the source
  chat's own message files). `attachment_render_path` emits it and
  `copy_media_into_output` copies the file to the same relative path under the
  output dir via `media_copy::copy_guarded`. Reject/skip absolute paths, schemes,
  backslashes, raw `..`, and percent-encoded traversal; a missing source is skipped
  (best-effort link). Thumbnails are not copied because the renderer never emits
  them — **coupling**: if the renderer grows thumbnail rendering, extend
  `copy_media_into_output` to also copy `thumbnail_path`, or those links will be dead.
- An HTML-imported DB stores `relative_path` with the per-chat dir prefix
  (`chat_001/…`); export-html links and copies media under that prefix. It is
  honest and resolves correctly — **do not strip it** (which prefix? JSON imports
  have none: `files/report.pdf`). A JSON-imported DB carries no such prefix.
- Reply links are emitted as `messages2.html#go_to_message{id}` and **must keep
  the `#go_to_message{id}` form**: the parser reads them back via
  `a[href*="go_to_message"]` (`extract_reply_id`), so emitting the naive
  `#message{id}` would silently drop `reply_to_message_id` on round-trip (guarded
  by integration test `exported_html_reimports_with_same_core_semantics`). Non-JS
  reply nav is dead in real tdesktop too; the only fidelity-safe polish left is
  the optional JS `CheckLocation` onload stub in `assets.rs` for `#go_to_message`
  deep-linking — low value. Do not "simplify" to `#message{id}`.
- Preserve service-event row/type round trips even when exact Telegram wording
  cannot be reconstructed from missing JSON actor/title/member data.

## LLM Export Notes

- `export-llm` produces a **lossy, token-economical Markdown view**, not a
  fidelity export, and is intentionally **not re-importable** (no round-trip
  test). It reuses the shared `export_rows` loader with a different renderer.
- `parse_utc` is a shared `time.rs` primitive used by both renderers.
- Output goes to a file (atomic temp+rename) or stdout (`-`); the stats summary
  is written to stderr so piped stdout stays clean.
- `export-llm --transcribe "<cmd>"` transcribes attachment kinds `voice`,
  `voice_message`, and `video_message` via an external command. Parsing lives in
  `src/llm_export/transcribe.rs` (`shell-words` → argv, run directly, no shell;
  `{}` = whole-argument path placeholder). It is output-only: no schema change,
  no persistence, DB stays read-only. Per-file failures degrade to the bare
  placeholder and are counted. Like `bundle.rs`, it canonicalizes each resolved
  audio path and skips any that escapes the export dir via a symlink, and also
  skips any hard link (link count > 1) whose canonical path stays inside the
  export but whose content could be an outside file — otherwise a crafted export
  could aim a voice file at an arbitrary path (e.g. `~/.ssh/id_rsa`) and
  exfiltrate it through the user's transcription engine. v1 is sequential, no
  timeout, no cache. The end-to-end test uses a fake `cat` engine over
  `tests/fixtures/voice_export`; the symlink- and hard-link-escape guards have
  their own `#[cfg(unix)]` tests.

## Design Rationale

Non-obvious *why* behind choices that are easy to second-guess or break by
accident. (Distilled from the original `export-llm` and voice-transcription
design specs, which are no longer kept as standalone documents.)

- **The LLM export is a deliberate lossy view.** `export-llm` breaks the
  no-data-loss rule *on purpose*: SQLite is the durable, lossless artifact and
  the Markdown is a derived, throwaway view tuned for token economy — everything
  it drops stays queryable in SQLite. It is named `export-llm`, not `export-md`,
  to signal a purpose-built lossy view rather than a faithful Markdown rendering.
- **Verbatim message text is safe without Markdown escaping.** The LLM renderer
  emits content unescaped, which is safe *only* because of the line shape: every
  line starts with `HH:MM`/name and every wrapped physical line is indented under
  the text column, so `#`, `-`, `>`, `*`, `_` inside content cannot be misparsed
  as document structure. Preserve that invariant — emitting content at column 0,
  or dropping the wrap indent, would break the assumption and reintroduce a need
  to escape. (Link-target safety is separate and *is* enforced: `TextUrl` hrefs
  still pass `media_path::safe_href`'s scheme allowlist; only text-content
  escaping is skipped.)
- **Nested rich-text runs are collapsed deterministically.** The entity extractor
  emits one entity per active mark over the same substring, so a bold link arrives
  as two byte-identical `.text` entities. The renderer merges a maximal run of
  identical-`.text` entities into a single wrapping (bold link → `[**link**](url)`)
  with fixed innermost→outermost precedence (code, bold, italic, strike, spoiler,
  link) so output is stable and testable. The rare false-merge — two genuinely
  separate, identical, adjacent, differently-styled words — is an accepted cost.
- **Reply anchors are reference-only `#n`.** Only messages that are actually
  replied to get a compact per-document id (`#1`, `#2`, … in first-appearance
  order), never the bulky raw Telegram message id; messages never replied to
  carry none. This keeps the document lean while still resolving `↳#n` replies.
- **The token estimate is intentionally tokenizer-free.** `≈ ceil(chars / 4)`,
  labelled approximate. The target model varies (Claude, GPT, …); a real
  tokenizer is model-specific and heavy, and ~4 chars/token is an adequate
  "will this fit the context window?" gauge.
- **Transcription is output-only by deliberate reversal.** The first design
  sketched persisting transcripts in SQLite so the exporter stayed a pure DB
  view. That was rejected: transcription lives entirely inside `export-llm` with
  no schema change and no cache (`SCHEMA_VERSION` stays `1`), so existing
  databases need no migration and the DB stays read-only. The cost — re-running
  the engine on every export — is an accepted v1 trade-off; persistence and
  caching remain open future work.
- **LLM-export tunables most likely to be revisited:** the participant list caps
  at 30 names then `+N more`; the token divisor is 4; all service events are
  included (they are already terse one-liners); the nested-run false-merge is
  accepted rather than disambiguated.

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

`Cargo.lock` is not committed, so a fresh checkout resolves the newest deps —
e.g. `libsqlite3-sys` ≥ 0.38, whose build script uses `cfg_select!` and so needs
rustc ≥ 1.97 to build. The tree is formatted with that toolchain's rustfmt, so a
plain `cargo fmt --check` (the default stable) is authoritative; don't reach for
an older toolchain's rustfmt, which would want to re-wrap the baseline.

CI parity matters — the three checks above are a subset of what
`.github/workflows/main.yml` (on `ubuntu-latest`) enforces, so passing them
locally is necessary but not sufficient:

- **The fmt gate is stricter.** CI runs
  `cargo fmt --all --check -- --config=imports_granularity=Crate`. The tree
  already satisfies it (imports are merged into one `use crate::{…}` /
  `use std::{…}` per module), but the plain `cargo fmt --check` above uses
  default (Preserve) granularity and will **not** flag a newly *split* import
  that CI then rejects. Keep imports merged, and run the CI form before claiming
  fmt is clean.
- **clippy/test run through `cargo-hack`:** `cargo hack clippy --workspace
  --each-feature -- -D warnings` and `cargo hack test --workspace --each-feature`.
  The crate declares no features, so `--each-feature` is a single pass, and the
  local `cargo clippy --all-targets -- -D warnings` above is a strict superset
  (it also lints the test/example targets, which CI's clippy does not).
- **CI is Linux-only.** The `#[cfg(not(unix))]` branches (in `db_swap.rs` and
  `output_dir.rs`) are never built or tested there, so a break in them passes CI
  silently — a local `--target x86_64-pc-windows-*` check is the only way to
  catch it.

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

## Shared Helpers

Previously-duplicated helpers are now centralized; keep them shared rather than
re-inlining a second copy:

- Reaction/emoji parsing (`reaction_emoji`, `reaction_count`, and the private
  `named_emoji`) lives in `src/reactions.rs`. Only the top-level
  `render_reactions*` wrappers legitimately differ between
  `src/html_export/render.rs` and `src/llm_export/render.rs`, because the two
  renderers format reactions differently.
- The single-file temp-DB-swap machinery (`temporary_database_path`,
  `replace_database`, `cleanup_temp_database`, and the private
  `temporary_backup_path`/`path_with_suffix`) lives in `src/db_swap.rs`, shared
  by `src/importer.rs` and `src/merge.rs`. It is the file-level analog of
  `output_dir.rs`'s directory-level swap (which is shared by bundle import and
  HTML export).
- The whitespace-normalizer is centralized as `text.rs::normalize_ws`.

## Open Work

Known-open directions, not yet started. Each is discussed in context in the
section named; this list is just the index.

- **Persist + cache voice/video transcripts** (`export-llm --transcribe`).
  Transcription is output-only today and re-runs the external engine on every
  export; persisting results in SQLite would need a `SCHEMA_VERSION` bump plus
  cache-invalidation design (keyed on file content). See *LLM Export Notes* and
  *Design Rationale*.
- **Parallelize transcription and add a per-file timeout.** v1 is sequential
  with no timeout, so a large chat is slow and a wedged engine hangs the whole
  export. See *LLM Export Notes*.
- **Extend the bundle output model to `merge` and `export-html`.** Only `import`
  builds a portable bundle (sibling temp dir + atomic swap, `assets/`); `merge`
  and `export-html` still take/produce a bare, caller-named DB path. See
  *Import Notes* (last bullet).
