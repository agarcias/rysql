# Changelog

All notable changes to RySQL will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Phase 6 packaging: `cargo-deb` metadata, AUR `PKGBUILD`, AppImage build
  script, `cargo-generate-rpm` metadata, AppStream metainfo, desktop file
  and icon under `packaging/linux/`.
- `.github/workflows/release.yml` triggered on `v*` tags: builds a Linux
  `.deb`, an AppImage and a Windows `.exe` (cross-compiled from Linux via
  `cargo-xwin`) and publishes them as GitHub release assets.
- `README.md` and `CHANGELOG.md` at the repo root.

## [0.6.0] — 2026-05-24

### Added — Phase 5: UX & robustness
- Friendly MySQL/MariaDB error messages for the most common codes
  (connection 1045 / 1130 / 2002 / 2003 / 2006 / 2013, schema 1044 / 1046
  / 1049 / 1051 / 1054 / 1146 / 1305 / 1364, constraints 1062 / 1048 /
  1216 / 1217 / 1451 / 1452, locking 1205 / 1213, syntax/privileges 1064
  / 1142 / 1149 / 1227); raw driver messages preserved in logs.
- App settings persisted in `settings.toml` (theme + history limit).
- View → Theme menu (Follow system / Light / Dark) backed by
  `egui::ThemePreference`.
- Persistent searchable query history (`history.json`, capped, deduped).
- Edit → History… modal with substring filter, "Load into editor",
  "Copy SQL" and "Clear all" actions.
- `Bridge::spawn_stream` + `UiEvent::StreamFinished` enabling a
  cancellable streaming task per ad-hoc execution. Status-bar spinner
  with a "Cancel" button while a stream is alive.
- Daily-rotated file logs via `tracing-appender` (cache dir) alongside
  stdout, and a panic hook that writes a timestamped backtrace to
  `cache/crashes/panic-<ts>.txt`.

### Added — Phase 4b: results polish
- `Cell::Blob(Vec<u8>)` retains full bytes for the new viewer.
- `ColumnMeta.origin` + `original_name` populated from sqlx's
  `ColumnOrigin::Table`, enabling single-table detection.
- `primary_key_columns` lookup via `information_schema.KEY_COLUMN_USAGE`
  and a `DbActor::PrimaryKey` command.
- Auto-`LIMIT 1000` for SELECT-like statements without an existing
  `LIMIT`; "Fetch next 1000" footer button on result tabs.
- BLOB viewer modal with hex+ASCII dump (4 KB cap, elision note) and
  "Copy hex" / "Copy as text (lossy)" actions.
- Copy result tabs to clipboard as CSV, TSV or SQL `INSERT` statements
  via the tab context menu.
- Edit-in-place by PK: cells in single-table results expose an
  "Edit cell…" action that opens a modal, builds an
  `UPDATE … WHERE pk = …` statement, reuses the destructive-confirm
  modal (no type-to-confirm gate for cell edits) and on success patches
  the local cell.
- Multi-statement scripts (e.g. `USE db; SELECT …;`) now run via
  client-side splitting and `spawn_stream`, stopping at the first
  error.

### Fixed
- Pin the sqlx pool to a single connection so session state (`USE`,
  `SET`, temporary tables) persists across subsequent statements.

### Added — Phase 4a: results pane
- `rysql-db::query`: typed `Cell` decoder, `QueryResult`, `DbActor::Query`.
- `rysql-sql::is_query_returning_rows` routes SELECT-like statements to
  `Query` and the rest to `Execute`.
- `egui_extras::TableBuilder` results pane with virtualized rendering,
  resizable columns, header-click sort (None → Asc → Desc), typed cell
  rendering (NULL / BLOB / JSON / Text), right-click "Copy value", and
  a horizontal scroll for very wide results.
- Sidebar object names truncate with ellipsis + tooltip on overflow.

### Added — Phase 3: SQL editor
- Inter and JetBrains Mono fonts bundled via `include_bytes!`.
- `rysql-sql` modules for syntect highlighting, statement splitting and
  ~150 MySQL/MariaDB keywords.
- Multi-tab `TextEdit::multiline().code_editor()` with a custom layouter
  that paints syntect-colored runs, `Ctrl+Enter` / `Ctrl+Shift+Enter` /
  `Ctrl+/` / `Ctrl+Shift+F` / `Ctrl+T` / `Ctrl+W` shortcuts.
- Cursor / selection capture via `TextEdit::show`, precise
  statement-under-cursor execution, selection-aware comment toggle.
- Floating autocomplete popup (schema names + keywords) with
  `Tab` / `Esc` / arrow-key handling.

### Added — Phase 2: schema browser
- information_schema-backed tree of databases → tables / views /
  procedures / functions / triggers / events.
- Lazy load per database; refresh per node.
- Context menus: copy name, copy CREATE, DROP / TRUNCATE / DROP DATABASE
  behind a type-to-confirm modal.

### Added — Phase 1: connections
- `ProfileStore` (TOML in XDG / AppData) + `keyring` v3 for passwords.
- `sqlx` 0.9 MySQL pool with TLS, sockets and sane interactive timeouts.
- `DbActor` + `DbHandle` (Ping / ServerInfo / Execute) with mpsc + oneshot.
- Tokio runtime on its own thread; `Bridge` for async ↔ egui events.
- New Connection modal with Test / Save and a connection sidebar with
  connect / disconnect / delete.

### Added — Phase 0: scaffold
- Cargo workspace with `rysql-core` / `rysql-db` / `rysql-sql` /
  `rysql-ui`.
- eframe shell with menu and status bar.
- GitHub Actions CI: fmt, clippy `-D warnings`, test, Windows
  cross-build via `cargo-xwin`.

[Unreleased]: https://github.com/arongarcia/rysql/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/arongarcia/rysql/releases/tag/v0.6.0
