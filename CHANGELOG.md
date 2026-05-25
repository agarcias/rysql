# Changelog

All notable changes to RySQL will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Editable Object inspector + editable Results tabs
- *Source* of procedures, functions and views is editable in place
  (`Edit` / `Save` / `Revert`). Save goes through a new
  `DbHandle::replace_routine` actor command that runs an optional
  `DROP` followed by the user's CREATE body as a single
  `sqlx::query`, bypassing the client-side statement splitter so
  semicolons inside `BEGIN … END` survive. DDL is not transactional
  in MySQL/MariaDB — if the CREATE fails after a successful DROP,
  the routine stays dropped until the user re-saves, and the
  confirm modal calls that out before the user commits.
- Results tabs that originate from a single table gain a row-edit
  toolkit:
  - `+ Add row` opens a per-column Insert modal with NULL / DEFAULT
    radios, lazy-loaded column metadata, and a live SQL preview.
  - A leading checkbox column lets the user select rows; right-click
    on a cell exposes `Delete row…`. Bulk actions appear in the
    toolbar — `Delete N row(s)…` (single-PK `IN (…)` or
    composite-PK tuple form) and, with ≥ 2 rows selected,
    `Bulk edit column…` for a single UPDATE across the selection.
  - Successful INSERT refreshes the result tab (Data subtab via
    LoadState invalidation; generic tabs via re-issuing the
    original SELECT and replacing rows in place); DELETE and bulk
    UPDATE patch the local grid in place to preserve pagination.
- *Structure* of tables exposes `+ Add column` and per-row `Edit`
  and `Drop` buttons. The modal handles both ADD COLUMN and
  CHANGE COLUMN (Modify mode is prefilled with the current
  properties) and emits a single ALTER TABLE; the dialog shows a
  live SQL preview. DROP COLUMN reuses the type-to-confirm gate so
  the user has to type the column name. After every column ALTER
  the Object view invalidates columns / indexes / foreign_keys /
  data so the next render re-fetches.
- Bridge-side plumbing: `UiEvent::TabColumnsLoaded` and
  `UiEvent::TabRefreshed`; new `ExecKind::{InsertedRow,
  DeletedRows, BulkUpdated, ReplaceSource, AlteredColumns}` plus
  matching `PendingExec` variants that route through the existing
  destructive-confirm modal. `DbCommand::ReplaceRoutine` and
  `DbHandle::replace_routine` added to `rysql-db`.
- New modules `column_dialog` (Add / Modify column form) on the UI
  side. The Insert / Bulk Update / Column edit modals all share the
  pattern of building the SQL during render, gating Submit on it,
  and surfacing it as the destructive-confirm preview so the user
  always sees the exact statement before clicking through.

### Added — Dock layout (editor + results + object inspector)
- Central panel migrated to `egui_dock` 0.19. Editor buffers, result sets
  and object inspectors now live as draggable tabs in a single dock area;
  any tab can be dropped on another to create a horizontal or vertical
  split, so the user can lay out editor / results side-by-side.
- Double-clicking a schema-tree item (or its new "Open" context-menu
  entry) opens an Object inspector tab with three subtabs:
  - **Structure** — compact tables of columns (name, type, nullable,
    default, PK, extra, comment) and indexes (name, columns, unique,
    type), plus a collapsible Foreign keys section sourced from
    `information_schema.KEY_COLUMN_USAGE ⨝ REFERENTIAL_CONSTRAINTS`.
  - **Data** — auto-issued `SELECT * FROM db.tbl LIMIT 1000` rendered
    inside the Object tab (no sibling Results tab); sort, fetch-next-1000
    and edit-in-place by PK keep working.
  - **Source** — the matching `SHOW CREATE …` in a read-only syntax-
    highlighted view.
  Procedures, functions, triggers and events expose only the Source
  subtab and default to it. Re-opening the same object focuses the
  existing tab instead of duplicating it.
- `Ctrl+W` and File → Close current tab now close whichever tab has
  focus, regardless of kind. New View entries: Close all SQL tabs / Close
  all results tabs. File → New SQL tab mirrors `Ctrl+T`.
- Hover-tooltip on tab buttons: Object tabs show `<Kind> · db.name`;
  Results tabs show the original SQL.
- UX safety net: closing the last tab (via any path) seeds a fresh empty
  `query-1` editor so the dock is never empty.

### Added
- Phase 6 packaging: `cargo-deb` metadata, AUR `PKGBUILD`, AppImage build
  script, `cargo-generate-rpm` metadata, AppStream metainfo, desktop file
  and icon under `packaging/linux/`.
- `.github/workflows/release.yml` triggered on `v*` tags: builds a Linux
  `.deb`, an AppImage and a Windows `.exe` (cross-compiled from Linux via
  `cargo-xwin`) and publishes them as GitHub release assets.
- `README.md` and `CHANGELOG.md` at the repo root.

### Changed
- The fixed bottom results panel is gone — result sets land as
  `DockTab::Results` tabs in the focused leaf instead. Past the 16-tab
  cap the oldest result tab is evicted and removed from the dock too.
- `rysql-db::schema` gains `ColumnInfo`, `IndexInfo`, `ForeignKeyInfo`
  types and `list_columns` / `list_indexes` / `list_foreign_keys` async
  helpers, mirrored as `DbActor` commands. `ObjectKind` now derives
  `Hash` so it can key the UI-side object map.

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
