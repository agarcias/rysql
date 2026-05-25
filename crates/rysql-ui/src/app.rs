use std::collections::{HashMap, HashSet};

use eframe::egui;
use egui_dock::{DockArea, DockState, Style};
use rysql_core::{
    secret, store::ProfileStore, AppSettings, ConnectionProfile, HistoryEntry, HistoryStore,
    SettingsStore, ThemeChoice,
};
use rysql_db::{build_pool, test_connection, DbActor, ObjectKind};
use rysql_sql::Highlighter;
use tokio::task::AbortHandle;

use crate::bridge::{Bridge, ExecKind, UiEvent};
use crate::column_dialog::{self, ColumnEditChoice, ColumnEditMode, ColumnEditState};
use crate::dialog::{self, ConfirmChoice, DialogAction, NewConnectionDialog, TestOutcome};
use crate::dock::{AppViewer, DockAction, DockTab};
use crate::editor::{self, EditorAction, EditorState};
use crate::history_view::{self, HistoryAction, HistoryView};
use crate::object_view::{self, ObjectAction, ObjectViewState};
#[allow(unused_imports)]
use crate::results::EditRequest;
use crate::results::{
    self, BulkUpdateChoice, EditCellState, EditDialogChoice, ExportFormat, InsertChoice,
    InsertRowState, ResultTab, ResultsAction, ResultsState,
};
use crate::sidebar::{self, SidebarAction, SidebarInput};
use crate::state::{
    ActiveConnection, ConfirmAction, LoadState, ObjectKey, PendingExec, SchemaState,
};

pub struct RysqlApp {
    bridge: Bridge,
    store: ProfileStore,
    profiles: Vec<ConnectionProfile>,
    dialog: Option<NewConnectionDialog>,
    in_flight: HashSet<String>,
    active: Option<ActiveConnection>,
    last_error: Option<String>,
    last_info: Option<String>,
    confirm: Option<ConfirmAction>,
    confirm_typed: String,
    editor: EditorState,
    dock: DockState<DockTab>,
    results: ResultsState,
    objects: HashMap<ObjectKey, ObjectViewState>,
    /// Dedicated highlighter for the Source subtab so the editor's
    /// highlighter can be borrowed independently each frame.
    source_highlighter: Highlighter,
    /// Open instance of the column add/modify modal (one at a time).
    column_edit_modal: Option<ColumnEditState>,
    /// Substring filter applied to the schema sidebar. UI-side only —
    /// persists across reconnects (lives on the app, not on
    /// `ActiveConnection`).
    schema_filter: String,
    /// Set by any close path (× button, Ctrl+W, menu, bulk-close);
    /// consumed at the end of the frame. When true, if the now-focused
    /// dock tab is a SQL editor, its `TextEdit` gets keyboard focus on
    /// the next frame.
    pending_editor_focus: bool,
    settings: AppSettings,
    settings_store: SettingsStore,
    history_store: HistoryStore,
    history_view: HistoryView,
    /// Abort handle for the currently running ad-hoc script, if any.
    in_flight_query: Option<AbortHandle>,
}

impl RysqlApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_fonts(crate::fonts::definitions());

        let bridge = Bridge::new(crate::runtime::handle(), cc.egui_ctx.clone());
        let store = ProfileStore::locate().expect("locate config dir");
        let profiles = match store.load() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load profiles, starting empty");
                Vec::new()
            }
        };

        let settings_store = SettingsStore::locate().expect("locate config dir");
        let settings = settings_store.load().unwrap_or_default();
        apply_theme(&cc.egui_ctx, settings.theme);

        let history_store =
            HistoryStore::locate(settings.history_limit.max(1)).expect("locate data dir");

        let editor = EditorState::default();
        // `EditorState::default()` seeds one buffer ("query-1"); the dock
        // boots with a matching `SqlEditor` tab so the user sees the same
        // empty editor at startup as before the refactor.
        let initial_buffer_id = editor
            .buffers
            .first()
            .map(|b| b.id)
            .expect("EditorState::default seeds one buffer");
        let dock = DockState::new(vec![DockTab::SqlEditor {
            buffer_id: initial_buffer_id,
        }]);

        Self {
            bridge,
            store,
            profiles,
            dialog: None,
            in_flight: HashSet::new(),
            active: None,
            last_error: None,
            last_info: None,
            confirm: None,
            confirm_typed: String::new(),
            editor,
            dock,
            results: ResultsState::default(),
            objects: HashMap::new(),
            source_highlighter: Highlighter::new_dark(),
            column_edit_modal: None,
            schema_filter: String::new(),
            pending_editor_focus: false,
            settings,
            settings_store,
            history_store,
            history_view: HistoryView::default(),
            in_flight_query: None,
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        for event in self.bridge.drain() {
            match event {
                UiEvent::TestResult { profile, result } => {
                    self.in_flight.remove(&profile);
                    if let Some(dialog) = self.dialog.as_mut() {
                        dialog.last_test = Some(match result {
                            Ok(d) => TestOutcome::Ok(format!("Connected in {:.1?}", d)),
                            Err(e) => TestOutcome::Err(format!("Failed: {e}")),
                        });
                    }
                }
                UiEvent::Connected {
                    profile,
                    handle,
                    info,
                } => {
                    self.in_flight.remove(&profile);
                    if let Some(prev) = self.active.replace(ActiveConnection {
                        profile_name: profile.clone(),
                        handle,
                        info,
                        schema: SchemaState::default(),
                    }) {
                        prev.handle.shutdown();
                    }
                    self.last_error = None;
                    self.fetch_databases();
                }
                UiEvent::ConnectFailed { profile, error } => {
                    self.in_flight.remove(&profile);
                    self.last_error = Some(format!("{profile}: {error}"));
                }
                UiEvent::DatabasesListed { profile, result } => {
                    if let Some(active) = self.active.as_mut() {
                        if active.profile_name == profile {
                            active.schema.databases = match result {
                                Ok(dbs) => LoadState::Loaded(dbs),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ObjectsListed {
                    profile,
                    db,
                    result,
                } => {
                    if let Some(active) = self.active.as_mut() {
                        if active.profile_name == profile {
                            let entry = active
                                .schema
                                .objects
                                .entry(db)
                                .or_insert(LoadState::NotLoaded);
                            *entry = match result {
                                Ok(o) => LoadState::Loaded(o),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ShowCreate {
                    profile,
                    name,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) == Some(&profile) {
                        match result {
                            Ok(ddl) => {
                                ctx.copy_text(ddl);
                                self.last_info =
                                    Some(format!("Copied CREATE for `{name}` to clipboard"));
                            }
                            Err(e) => {
                                self.last_error = Some(format!("SHOW CREATE failed: {e}"));
                            }
                        }
                    }
                }
                UiEvent::ExecResult {
                    profile,
                    kind,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    match &result {
                        Ok(out) => {
                            self.last_error = None;
                            self.last_info = Some(format!(
                                "OK · {} row(s) affected · {:.1?}",
                                out.affected_rows, out.elapsed
                            ));
                            self.invalidate_after(&kind);
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Exec failed: {e}"));
                        }
                    }
                }
                UiEvent::QueryResult {
                    profile,
                    label,
                    sql,
                    page_size,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    let (success, summary) = match &result {
                        Ok(qr) => {
                            self.last_error = None;
                            let msg = format!(
                                "{} row(s) · {} col(s) · {:.1?}",
                                qr.rows.len(),
                                qr.columns.len(),
                                qr.elapsed
                            );
                            self.last_info = Some(msg.clone());
                            (true, msg)
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Query failed: {e}"));
                            (false, format!("Query failed: {e}"))
                        }
                    };
                    self.push_history(&profile, &sql, success, &summary);
                    if let Ok(qr) = result {
                        let tab_id = self.results.next_tab_id();
                        let row_count = qr.rows.len() as u64;
                        let mut tab = ResultTab::new(tab_id, label, sql, qr);
                        tab.page_size = page_size;
                        tab.has_more = row_count >= page_size && page_size > 0;
                        if let Some(target) = tab.detect_single_table() {
                            self.fetch_primary_key(tab_id, target.db.clone(), target.table.clone());
                            tab.editable = Some(target);
                        }
                        if let Some(evicted_id) = self.results.push(tab) {
                            self.cleanup_evicted_result_tab(evicted_id);
                        }
                        self.dock.push_to_focused_leaf(DockTab::Results { tab_id });
                    }
                }
                UiEvent::PageAppended {
                    profile,
                    tab_id,
                    page_size,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    match result {
                        Ok(qr) => {
                            if let Some(idx) = self.results.find_by_id(tab_id) {
                                let added = qr.rows.len() as u64;
                                self.results.tabs[idx].append(qr, page_size);
                                self.last_info = Some(format!(
                                    "Appended {} row(s) · total {}",
                                    added,
                                    self.results.tabs[idx].result.rows.len()
                                ));
                            }
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Fetch more failed: {e}"));
                        }
                    }
                }
                UiEvent::PrimaryKey {
                    profile,
                    tab_id,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    if let Some(idx) = self.results.find_by_id(tab_id) {
                        match result {
                            Ok(pk) if !pk.is_empty() => {
                                if let Some(target) = self.results.tabs[idx].editable.as_mut() {
                                    target.pk_cols = pk;
                                }
                            }
                            _ => {
                                self.results.tabs[idx].editable = None;
                            }
                        }
                    }
                }
                UiEvent::StreamFinished => {
                    self.in_flight_query = None;
                }
                UiEvent::ObjectColumnsLoaded {
                    profile,
                    key,
                    result,
                } => {
                    if self.is_active_profile(&profile) {
                        if let Some(state) = self.objects.get_mut(&key) {
                            state.columns = match result {
                                Ok(v) => LoadState::Loaded(v),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ObjectIndexesLoaded {
                    profile,
                    key,
                    result,
                } => {
                    if self.is_active_profile(&profile) {
                        if let Some(state) = self.objects.get_mut(&key) {
                            state.indexes = match result {
                                Ok(v) => LoadState::Loaded(v),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ObjectForeignKeysLoaded {
                    profile,
                    key,
                    result,
                } => {
                    if self.is_active_profile(&profile) {
                        if let Some(state) = self.objects.get_mut(&key) {
                            state.foreign_keys = match result {
                                Ok(v) => LoadState::Loaded(v),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ObjectSourceLoaded {
                    profile,
                    key,
                    result,
                } => {
                    if self.is_active_profile(&profile) {
                        if let Some(state) = self.objects.get_mut(&key) {
                            state.source = match result {
                                Ok(v) => LoadState::Loaded(v),
                                Err(e) => LoadState::Error(e),
                            };
                        }
                    }
                }
                UiEvent::ObjectDataLoaded {
                    profile,
                    key,
                    label,
                    sql,
                    page_size,
                    result,
                } => {
                    if !self.is_active_profile(&profile) {
                        continue;
                    }
                    match result {
                        Err(e) => {
                            if let Some(state) = self.objects.get_mut(&key) {
                                state.data = LoadState::Error(e);
                            }
                        }
                        Ok(qr) => {
                            let tab_id = self.results.next_tab_id();
                            let row_count = qr.rows.len() as u64;
                            let mut tab = ResultTab::new(tab_id, label, sql, qr);
                            tab.page_size = page_size;
                            tab.has_more = row_count >= page_size && page_size > 0;
                            if let Some(target) = tab.detect_single_table() {
                                self.fetch_primary_key(
                                    tab_id,
                                    target.db.clone(),
                                    target.table.clone(),
                                );
                                tab.editable = Some(target);
                            }
                            if let Some(evicted_id) = self.results.push(tab) {
                                self.cleanup_evicted_result_tab(evicted_id);
                            }
                            if let Some(state) = self.objects.get_mut(&key) {
                                state.data = LoadState::Loaded(tab_id);
                            } else {
                                // Object tab was closed before the data
                                // landed; drop the orphan result.
                                self.results.remove_by_id(tab_id);
                            }
                        }
                    }
                }
                UiEvent::TabColumnsLoaded {
                    profile,
                    tab_id,
                    result,
                } => {
                    if !self.is_active_profile(&profile) {
                        continue;
                    }
                    let Some(modal) = self.results.insert_modal.as_mut() else {
                        continue;
                    };
                    if modal.tab_id != tab_id {
                        continue;
                    }
                    match result {
                        Ok(cols) => modal.seed_values(cols),
                        Err(e) => modal.columns = LoadState::Error(e),
                    }
                }
                UiEvent::TabRefreshed {
                    profile,
                    tab_id,
                    page_size,
                    result,
                } => {
                    if !self.is_active_profile(&profile) {
                        continue;
                    }
                    let Some(idx) = self.results.find_by_id(tab_id) else {
                        continue;
                    };
                    match result {
                        Ok(qr) => {
                            let row_count = qr.rows.len() as u64;
                            let tab = &mut self.results.tabs[idx];
                            tab.result = qr;
                            tab.order = (0..tab.result.rows.len()).collect();
                            tab.apply_sort();
                            tab.page_size = page_size;
                            tab.next_offset = row_count;
                            tab.has_more = row_count >= page_size && page_size > 0;
                            self.last_info = Some(format!("Refreshed · {row_count} row(s)"));
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Refresh failed: {e}"));
                        }
                    }
                }
                UiEvent::CellEdited {
                    profile,
                    tab_id,
                    row,
                    col,
                    new_value,
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    match result {
                        Ok(out) => {
                            if let Some(idx) = self.results.find_by_id(tab_id) {
                                let tab = &mut self.results.tabs[idx];
                                if let Some(cell) =
                                    tab.result.rows.get_mut(row).and_then(|r| r.get_mut(col))
                                {
                                    use rysql_db::Cell;
                                    *cell = if new_value.eq_ignore_ascii_case("NULL") {
                                        Cell::Null
                                    } else {
                                        Cell::Text(new_value)
                                    };
                                }
                                tab.apply_sort();
                            }
                            self.last_error = None;
                            self.last_info = Some(format!(
                                "Updated 1 cell · {} row(s) affected · {:.1?}",
                                out.affected_rows, out.elapsed
                            ));
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Update failed: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn invalidate_after(&mut self, kind: &ExecKind) {
        match kind {
            ExecKind::DroppedDatabase => {
                if let Some(active) = self.active.as_mut() {
                    active.schema.objects.clear();
                }
                self.fetch_databases();
            }
            ExecKind::AlteredDb(db) => {
                if let Some(active) = self.active.as_mut() {
                    active.schema.objects.remove(db);
                }
            }
            ExecKind::Adhoc => {}
            ExecKind::ReplaceSource(key) => {
                // Drop the cached DDL so the next render re-issues
                // `SHOW CREATE` and we display whatever the server
                // normalised the body to.
                if let Some(state) = self.objects.get_mut(key) {
                    state.source = LoadState::NotLoaded;
                    state.source_editing = false;
                    state.source_buffer.clear();
                    state.source_original.clear();
                }
            }
            ExecKind::InsertedRow { tab_id } => {
                self.refresh_after_row_insert(*tab_id);
            }
            ExecKind::DeletedRows { tab_id, rows } => {
                self.apply_local_delete(*tab_id, rows);
            }
            ExecKind::BulkUpdated {
                tab_id,
                rows,
                col_idx,
                new_value,
            } => {
                self.apply_local_bulk_update(*tab_id, rows, *col_idx, new_value);
            }
            ExecKind::AlteredColumns(key) => {
                // ALTER COLUMN may shift PKs, defaults and auto_increment,
                // and the existing Data result is now stale (columns may
                // have changed). Invalidate everything the Object view
                // owns so the next render re-fetches.
                let stale_data_tab = self.objects.get(key).and_then(|s| s.data_tab_id());
                if let Some(tab_id) = stale_data_tab {
                    self.results.remove_by_id(tab_id);
                }
                if let Some(state) = self.objects.get_mut(key) {
                    state.columns = LoadState::NotLoaded;
                    state.indexes = LoadState::NotLoaded;
                    state.foreign_keys = LoadState::NotLoaded;
                    state.data = LoadState::NotLoaded;
                }
            }
        }
    }

    /// Patch every selected row's column with `new_value`. No server
    /// round-trip: we trust the server accepted the UPDATE (otherwise we
    /// would have stayed in the `Err` branch of `handle_events::ExecResult`).
    fn apply_local_bulk_update(
        &mut self,
        tab_id: u64,
        rows: &[usize],
        col_idx: usize,
        new_value: &str,
    ) {
        use rysql_db::Cell;
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let tab = &mut self.results.tabs[idx];
        let new_cell = if new_value.eq_ignore_ascii_case("NULL") {
            Cell::Null
        } else {
            Cell::Text(new_value.to_string())
        };
        for &row_idx in rows {
            if let Some(row) = tab.result.rows.get_mut(row_idx) {
                if let Some(cell) = row.get_mut(col_idx) {
                    *cell = new_cell.clone();
                }
            }
        }
        tab.selection.clear();
        tab.apply_sort();
    }

    /// Drop the just-deleted rows from the local grid. No server refresh:
    /// we'd lose pagination and the rows are guaranteed gone from the
    /// server side. Rebuilds `order` via `apply_sort` so any active sort
    /// is preserved.
    fn apply_local_delete(&mut self, tab_id: u64, rows: &[usize]) {
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let tab = &mut self.results.tabs[idx];
        let to_remove: std::collections::HashSet<usize> = rows.iter().copied().collect();
        let mut walking = 0usize;
        tab.result.rows.retain(|_| {
            let keep = !to_remove.contains(&walking);
            walking += 1;
            keep
        });
        // Local rows changed → indices invalidated; selection is stale,
        // sort order needs a rebuild.
        tab.selection.clear();
        tab.next_offset = tab.result.rows.len() as u64;
        tab.apply_sort();
    }

    /// Pick the right refresh strategy for the tab that just received an
    /// INSERT: for an Object-view Data subtab we invalidate its `data`
    /// LoadState (so the next render re-fetches via `load_object_data`);
    /// for a generic Results tab we re-issue the original SELECT and
    /// replace rows in place via `UiEvent::TabRefreshed`.
    fn refresh_after_row_insert(&mut self, tab_id: u64) {
        let data_subtab_key = self
            .objects
            .iter()
            .find(|(_, state)| state.data_tab_id() == Some(tab_id))
            .map(|(k, _)| k.clone());
        if let Some(key) = data_subtab_key {
            if let Some(state) = self.objects.get_mut(&key) {
                state.data = LoadState::NotLoaded;
            }
            self.results.remove_by_id(tab_id);
            return;
        }
        // Generic Results tab: re-issue tab.sql at offset 0.
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let tab = &self.results.tabs[idx];
        let sql = tab.sql.clone();
        let page_size = if tab.page_size == 0 {
            results::DEFAULT_PAGE_SIZE
        } else {
            tab.page_size
        };
        let exec_sql = if rysql_sql::has_limit_clause(&sql) {
            sql.clone()
        } else {
            rysql_sql::inject_pagination(&sql, page_size, 0)
        };
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        self.bridge.spawn(async move {
            let result = handle.query(exec_sql).await.map_err(|e| e.friendly());
            UiEvent::TabRefreshed {
                profile,
                tab_id,
                page_size,
                result,
            }
        });
    }

    fn run_adhoc(&mut self, sql: String) {
        let Some(active) = self.active.as_ref() else {
            self.last_error = Some("Not connected".into());
            return;
        };

        // sqlx's prepared-statement protocol only accepts ONE statement per
        // call, so we split the buffer client-side. Each statement runs
        // sequentially and emits its own event so multi-statement scripts
        // like `USE db; SELECT …;` work as expected.
        let ranges = rysql_sql::split_statements(&sql);
        let statements: Vec<String> = if ranges.is_empty() {
            let trimmed = sql.trim();
            if trimmed.is_empty() {
                return;
            }
            vec![trimmed.to_string()]
        } else {
            ranges
                .into_iter()
                .map(|r| sql[r].trim().trim_end_matches(';').trim_end().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        if statements.is_empty() {
            return;
        }

        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let page_size = results::DEFAULT_PAGE_SIZE;

        // Replace any previous in-flight stream so its abort handle is dropped.
        self.in_flight_query = None;
        let abort = self.bridge.spawn_stream(move |emitter| async move {
            for stmt in statements {
                let label = sql_label(&stmt);
                if rysql_sql::is_query_returning_rows(&stmt) {
                    let user_sql = stmt.clone();
                    let exec_sql = if rysql_sql::has_limit_clause(&stmt) {
                        stmt
                    } else {
                        rysql_sql::inject_pagination(&stmt, page_size, 0)
                    };
                    let result = handle.query(exec_sql).await.map_err(|e| e.friendly());
                    let is_err = result.is_err();
                    emitter.send(UiEvent::QueryResult {
                        profile: profile.clone(),
                        label,
                        sql: user_sql,
                        page_size,
                        result,
                    });
                    if is_err {
                        break;
                    }
                } else {
                    let result = handle.execute(stmt).await.map_err(|e| e.friendly());
                    let is_err = result.is_err();
                    emitter.send(UiEvent::ExecResult {
                        profile: profile.clone(),
                        kind: ExecKind::Adhoc,
                        result,
                    });
                    if is_err {
                        break;
                    }
                }
            }
        });
        self.in_flight_query = Some(abort);
    }

    fn push_history(&mut self, profile: &str, sql: &str, success: bool, summary: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }
        let entry = HistoryEntry {
            timestamp: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            profile: profile.to_string(),
            sql: trimmed.to_string(),
            success,
            summary: summary.to_string(),
        };
        if let Err(e) = self.history_store.push(entry) {
            tracing::warn!(error = %e, "failed to append to history");
        } else {
            // Invalidate the cached view so the next open re-reads the file.
            self.history_view.entries.clear();
        }
    }

    fn cancel_in_flight(&mut self) {
        if let Some(abort) = self.in_flight_query.take() {
            abort.abort();
            self.last_info = Some("Cancelled".into());
        }
    }

    fn fetch_primary_key(&mut self, tab_id: u64, db: String, table: String) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        self.bridge.spawn(async move {
            let result = handle
                .primary_key(db, table)
                .await
                .map_err(|e| e.friendly());
            UiEvent::PrimaryKey {
                profile,
                tab_id,
                result,
            }
        });
    }

    fn fetch_more(&mut self, tab_id: u64, sql: String, offset: u64, limit: u64) {
        let Some(active) = self.active.as_ref() else {
            self.last_error = Some("Not connected".into());
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let exec_sql = rysql_sql::inject_pagination(&sql, limit, offset);
        self.bridge.spawn(async move {
            let result = handle.query(exec_sql).await.map_err(|e| e.friendly());
            UiEvent::PageAppended {
                profile,
                tab_id,
                page_size: limit,
                result,
            }
        });
    }

    fn run_edit(&mut self, req: results::EditRequest) {
        let Some(active) = self.active.as_ref() else {
            self.last_error = Some("Not connected".into());
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let sql = req.sql.clone();
        let tab_id = req.tab_id;
        let row = req.row;
        let col = req.col;
        let new_value = req.new_value.clone();
        self.bridge.spawn(async move {
            let result = handle.execute(sql).await.map_err(|e| e.friendly());
            UiEvent::CellEdited {
                profile,
                tab_id,
                row,
                col,
                new_value,
                result,
            }
        });
    }

    fn apply_results_actions(&mut self, ctx: &egui::Context, actions: Vec<ResultsAction>) {
        for action in actions {
            match action {
                ResultsAction::CopyText(text) => {
                    ctx.copy_text(text.clone());
                    self.last_info = Some(format!("Copied: {text}"));
                }
                ResultsAction::FetchMore {
                    tab_id,
                    sql,
                    offset,
                    limit,
                } => self.fetch_more(tab_id, sql, offset, limit),
                ResultsAction::Export { tab_id, format } => {
                    if let Some(idx) = self.results.find_by_id(tab_id) {
                        let tab = &self.results.tabs[idx];
                        let payload = results::export(tab, format);
                        ctx.copy_text(payload);
                        let what = match format {
                            ExportFormat::Csv => "CSV",
                            ExportFormat::Tsv => "TSV",
                            ExportFormat::Insert => "INSERT statements",
                        };
                        self.last_info = Some(format!(
                            "Copied {} row(s) as {} to clipboard",
                            tab.result.rows.len(),
                            what
                        ));
                    }
                }
                ResultsAction::OpenEdit {
                    tab_id,
                    row,
                    col,
                    column_name,
                    original_value,
                } => {
                    // Only open if the target tab is still editable.
                    let editable = self
                        .results
                        .find_by_id(tab_id)
                        .and_then(|i| self.results.tabs[i].editable.as_ref())
                        .is_some_and(|t| !t.pk_cols.is_empty());
                    if editable {
                        self.results.edit_modal = Some(EditCellState {
                            tab_id,
                            row,
                            col,
                            column_name,
                            new_value: original_value.clone(),
                            original_value,
                        });
                    } else {
                        self.last_info = Some("Cell not editable (no detected primary key)".into());
                    }
                }
                ResultsAction::ApplyEdit(req) => {
                    // First confirm via the existing destructive-confirm modal.
                    self.confirm = Some(ConfirmAction {
                        title: format!("Update `{}`", req.tab_id),
                        message: format!("Apply this UPDATE to row {} of the result?", req.row + 1),
                        sql: req.preview.clone(),
                        kind: PendingExec::EditCell(req),
                    });
                    self.confirm_typed.clear();
                }
                ResultsAction::OpenInsert { tab_id } => self.open_insert_modal(tab_id),
                ResultsAction::ApplyInlineEdit(req) => {
                    // Spreadsheet-style edit: skip the confirm modal and
                    // route straight to `run_edit`. The existing
                    // `UiEvent::CellEdited` handler patches the cell on
                    // success and surfaces server errors on the status
                    // bar — same as the modal path.
                    self.run_edit(req);
                }
                ResultsAction::OpenDeleteRow { tab_id, row } => {
                    self.open_delete_confirm(tab_id, vec![row]);
                }
                ResultsAction::OpenBulkDelete { tab_id } => {
                    let rows: Vec<usize> = match self.results.find_by_id(tab_id) {
                        Some(idx) => {
                            let mut rows: Vec<usize> =
                                self.results.tabs[idx].selection.iter().copied().collect();
                            rows.sort_unstable();
                            rows
                        }
                        None => continue,
                    };
                    if rows.is_empty() {
                        self.last_info = Some("No rows selected".into());
                        continue;
                    }
                    self.open_delete_confirm(tab_id, rows);
                }
                ResultsAction::OpenBulkUpdate { tab_id } => self.open_bulk_update_modal(tab_id),
            }
        }
    }

    /// Open the bulk-update modal for the rows currently in `tab.selection`.
    /// Seeds the column dropdown with the first non-PK column so the modal
    /// is usable on the first click.
    fn open_bulk_update_modal(&mut self, tab_id: u64) {
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let tab = &self.results.tabs[idx];
        let Some(target) = tab.editable.as_ref() else {
            self.last_info = Some("Result is not editable (need a single-table origin)".into());
            return;
        };
        if target.pk_cols.is_empty() {
            self.last_info = Some("No primary key detected — cannot bulk update".into());
            return;
        }
        let mut selected_rows: Vec<usize> = tab.selection.iter().copied().collect();
        selected_rows.sort_unstable();
        if selected_rows.len() < 2 {
            self.last_info = Some("Select at least 2 rows for a bulk update".into());
            return;
        }
        let target = target.clone();
        let columns = tab.result.columns.clone();
        let selected_col = columns
            .iter()
            .position(|c| !results::is_pk_column(&target, c));
        self.results.bulk_update_modal = Some(results::BulkUpdateState {
            tab_id,
            target,
            columns,
            selected_rows,
            selected_col,
            mode: results::BulkValueMode::Value,
            value: String::new(),
        });
    }

    /// Build the DELETE SQL for the given row indices and push the
    /// destructive-confirm modal. Used by both the single-row context-menu
    /// path and the bulk `Delete N rows…` toolbar button.
    fn open_delete_confirm(&mut self, tab_id: u64, rows: Vec<usize>) {
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let tab = &self.results.tabs[idx];
        let Some(target) = tab.editable.as_ref() else {
            self.last_info = Some("Result is not editable (need a single-table origin)".into());
            return;
        };
        if target.pk_cols.is_empty() {
            self.last_info = Some("No primary key detected — cannot delete".into());
            return;
        }
        match results::build_delete_sql(target, &tab.result.columns, &tab.result.rows, &rows) {
            Ok(sql) => {
                let count = rows.len();
                let title = if count == 1 {
                    format!("Delete row from `{}`.`{}`", target.db, target.table)
                } else {
                    format!(
                        "Delete {count} rows from `{}`.`{}`",
                        target.db, target.table
                    )
                };
                let message = if count == 1 {
                    "Apply this DELETE? The row will disappear from the grid \
                     immediately on success."
                        .to_string()
                } else {
                    format!(
                        "Apply this DELETE? {count} row(s) will disappear from \
                         the grid immediately on success."
                    )
                };
                self.confirm = Some(ConfirmAction {
                    title,
                    message,
                    sql,
                    kind: PendingExec::DeleteRows { tab_id, rows },
                });
                self.confirm_typed.clear();
            }
            Err(e) => {
                self.last_error = Some(format!("Cannot build DELETE: {e}"));
            }
        }
    }

    /// Open the Insert modal for `tab_id`. Requires the tab to be editable
    /// (single-table origin). Kicks off `list_columns` so the modal can
    /// populate; the user sees a spinner until that lands.
    fn open_insert_modal(&mut self, tab_id: u64) {
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let Some(target) = self.results.tabs[idx].editable.clone() else {
            self.last_info = Some("Result is not editable (need a single-table origin)".into());
            return;
        };
        self.results.insert_modal = Some(InsertRowState::new(tab_id, target.clone()));
        let Some(active) = self.active.as_ref() else {
            self.last_error = Some("Not connected".into());
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        self.bridge.spawn(async move {
            let result = handle
                .list_columns(target.db, target.table)
                .await
                .map_err(|e| e.friendly());
            UiEvent::TabColumnsLoaded {
                profile,
                tab_id,
                result,
            }
        });
    }

    fn collect_schema_names(&self) -> Vec<String> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for state in active.schema.objects.values() {
            if let LoadState::Loaded(objs) = state {
                out.extend(objs.tables.iter().cloned());
                out.extend(objs.views.iter().cloned());
                out.extend(objs.procedures.iter().cloned());
                out.extend(objs.functions.iter().cloned());
                out.extend(objs.triggers.iter().cloned());
                out.extend(objs.events.iter().cloned());
            }
        }
        if let LoadState::Loaded(dbs) = &active.schema.databases {
            out.extend(dbs.iter().cloned());
        }
        out.sort();
        out.dedup();
        out
    }

    fn apply_editor_actions(&mut self, actions: Vec<EditorAction>) {
        for action in actions {
            match action {
                EditorAction::NewTab => self.open_new_editor_tab(),
                EditorAction::CloseTab => self.close_focused_dock_tab(),
                EditorAction::Format => editor::apply_format(&mut self.editor),
                EditorAction::ToggleComment => editor::apply_toggle_comment(&mut self.editor),
                EditorAction::Execute(arg) => {
                    if let Some(sql) = editor::resolve_execute(&self.editor, &arg, None) {
                        self.run_adhoc(sql);
                    } else {
                        self.last_info = Some("Nothing to execute".into());
                    }
                }
            }
        }
    }

    fn open_new_editor_tab(&mut self) {
        let id = self.editor.new_buffer();
        self.dock
            .push_to_focused_leaf(DockTab::SqlEditor { buffer_id: id });
    }

    /// Resolve the `tab_id` of the result set behind whatever the user
    /// has focused: a `DockTab::Results` directly, or the Data subtab of
    /// a `DockTab::Object`. Returns `None` for editor tabs or for object
    /// tabs whose active subtab isn't Data / hasn't loaded yet.
    fn focused_data_tab_id(&self) -> Option<u64> {
        let node_path = self.dock.focused_leaf()?;
        let leaf = self.dock.leaf(node_path).ok()?;
        let tab = leaf.tabs.get(leaf.active.0)?;
        match tab {
            DockTab::Results { tab_id } => Some(*tab_id),
            DockTab::Object { key } => {
                let state = self.objects.get(key)?;
                if state.sub == object_view::SubTab::Data {
                    state.data_tab_id()
                } else {
                    None
                }
            }
            DockTab::SqlEditor { .. } => None,
        }
    }

    /// Ctrl+Del handler: route the focused tab's selection through the
    /// same `open_delete_confirm` path the toolbar button uses. Empty
    /// selection is a no-op (no point opening an empty confirm modal).
    fn delete_focused_selection(&mut self) {
        let Some(tab_id) = self.focused_data_tab_id() else {
            return;
        };
        let Some(idx) = self.results.find_by_id(tab_id) else {
            return;
        };
        let mut rows: Vec<usize> = self.results.tabs[idx].selection.iter().copied().collect();
        rows.sort_unstable();
        if rows.is_empty() {
            return;
        }
        self.open_delete_confirm(tab_id, rows);
    }

    /// Close the dock tab in the currently-focused leaf, regardless of
    /// variant. `on_close` on the viewer only fires when the user clicks the
    /// tab's close button — for shortcut-driven closes (`Ctrl+W`, menu) we
    /// drive both the dock removal and the matching state cleanup here.
    fn close_focused_dock_tab(&mut self) {
        let Some(node_path) = self.dock.focused_leaf() else {
            return;
        };
        let Ok(leaf) = self.dock.leaf(node_path) else {
            return;
        };
        let active_tab_idx = leaf.active;
        let Some(tab) = leaf.tabs.get(active_tab_idx.0).cloned() else {
            return;
        };
        let tab_path = egui_dock::TabPath::new(node_path.surface, node_path.node, active_tab_idx);
        self.dock.remove_tab(tab_path);
        self.dispose_tab_state(tab);
        self.pending_editor_focus = true;
    }

    /// Free whatever backing state lives outside the dock for a tab that
    /// has just been removed: the editor buffer, the result set, or the
    /// object inspector entry (and its embedded data result).
    fn dispose_tab_state(&mut self, tab: DockTab) {
        match tab {
            DockTab::SqlEditor { buffer_id } => self.editor.close_buffer_by_id(buffer_id),
            DockTab::Results { tab_id } => {
                self.results.remove_by_id(tab_id);
            }
            DockTab::Object { key } => {
                if let Some(state) = self.objects.remove(&key) {
                    if let Some(data_id) = state.data_tab_id() {
                        self.results.remove_by_id(data_id);
                    }
                }
            }
        }
    }

    /// Close every dock tab matching `predicate`. Uses a find-and-remove
    /// loop so tab indices never go stale between removals.
    fn close_all_dock_tabs_matching<F>(&mut self, predicate: F)
    where
        F: Fn(&DockTab) -> bool,
    {
        let mut closed_any = false;
        loop {
            let target = self.dock.iter_all_tabs().find_map(|(path, tab)| {
                if predicate(tab) {
                    Some((path, tab.clone()))
                } else {
                    None
                }
            });
            let Some((path, tab)) = target else { break };
            self.dock.remove_tab(path);
            self.dispose_tab_state(tab);
            closed_any = true;
        }
        if closed_any {
            self.pending_editor_focus = true;
        }
    }

    fn close_all_editor_tabs(&mut self) {
        self.close_all_dock_tabs_matching(|t| matches!(t, DockTab::SqlEditor { .. }));
    }

    fn close_all_results_tabs(&mut self) {
        self.close_all_dock_tabs_matching(|t| matches!(t, DockTab::Results { .. }));
    }

    fn apply_dock_actions(&mut self, actions: Vec<DockAction>) {
        let mut closed_any = false;
        for action in actions {
            match action {
                DockAction::CloseEditorBuffer(id) => {
                    closed_any = true;
                    self.editor.close_buffer_by_id(id);
                }
                DockAction::CloseResultsTab(id) => {
                    closed_any = true;
                    self.results.remove_by_id(id);
                }
                DockAction::CloseObjectView(key) => {
                    closed_any = true;
                    if let Some(state) = self.objects.remove(&key) {
                        if let Some(data_id) = state.data_tab_id() {
                            self.results.remove_by_id(data_id);
                        }
                    }
                }
                DockAction::ObjectRequest { key, action } => {
                    self.dispatch_object_request(key, action);
                }
                DockAction::FocusedEditor(id) => {
                    if let Some(idx) = self.editor.buffer_index(id) {
                        self.editor.active = idx;
                    }
                }
            }
        }
        if closed_any {
            self.pending_editor_focus = true;
        }
    }

    fn dispatch_object_request(&mut self, key: ObjectKey, action: ObjectAction) {
        if self.active.is_none() {
            // No live connection: mark every load as a friendly error so the
            // subtab doesn't spin forever, and surface Save attempts.
            if let Some(state) = self.objects.get_mut(&key) {
                let msg = "Not connected".to_string();
                match action {
                    ObjectAction::LoadColumns => state.columns = LoadState::Error(msg),
                    ObjectAction::LoadIndexes => state.indexes = LoadState::Error(msg),
                    ObjectAction::LoadForeignKeys => state.foreign_keys = LoadState::Error(msg),
                    ObjectAction::LoadSource => state.source = LoadState::Error(msg),
                    ObjectAction::LoadData => state.data = LoadState::Error(msg),
                    ObjectAction::SaveSource(_)
                    | ObjectAction::AddColumn
                    | ObjectAction::DropColumn { .. }
                    | ObjectAction::ModifyColumn { .. } => self.last_error = Some(msg),
                }
            }
            return;
        }
        let active = self.active.as_ref().unwrap();
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let db = key.db.clone();
        let name = key.name.clone();
        let kind = key.kind;
        match action {
            ObjectAction::LoadColumns => {
                let key = key.clone();
                self.bridge.spawn(async move {
                    let result = handle
                        .list_columns(db, name)
                        .await
                        .map_err(|e| e.friendly());
                    UiEvent::ObjectColumnsLoaded {
                        profile,
                        key,
                        result,
                    }
                });
            }
            ObjectAction::LoadIndexes => {
                let key = key.clone();
                self.bridge.spawn(async move {
                    let result = handle
                        .list_indexes(db, name)
                        .await
                        .map_err(|e| e.friendly());
                    UiEvent::ObjectIndexesLoaded {
                        profile,
                        key,
                        result,
                    }
                });
            }
            ObjectAction::LoadForeignKeys => {
                let key = key.clone();
                self.bridge.spawn(async move {
                    let result = handle
                        .list_foreign_keys(db, name)
                        .await
                        .map_err(|e| e.friendly());
                    UiEvent::ObjectForeignKeysLoaded {
                        profile,
                        key,
                        result,
                    }
                });
            }
            ObjectAction::LoadSource => {
                let key = key.clone();
                self.bridge.spawn(async move {
                    let result = handle
                        .show_create(db, kind, name)
                        .await
                        .map_err(|e| e.friendly());
                    UiEvent::ObjectSourceLoaded {
                        profile,
                        key,
                        result,
                    }
                });
            }
            ObjectAction::LoadData => self.load_object_data(key),
            ObjectAction::SaveSource(body) => self.enqueue_save_source(key, body),
            ObjectAction::AddColumn => {
                self.column_edit_modal = Some(ColumnEditState::new_add(key));
            }
            ObjectAction::DropColumn { name } => self.enqueue_drop_column(key, name),
            ObjectAction::ModifyColumn { name } => self.open_modify_column(key, name),
        }
    }

    /// Look up `name` in the Object view's cached columns and open the
    /// edit modal in Modify mode, prefilled with the current properties.
    /// Bails with a friendly message if the columns haven't loaded yet
    /// (which shouldn't happen — the user can only click the button when
    /// the row is visible).
    fn open_modify_column(&mut self, key: ObjectKey, name: String) {
        let Some(state) = self.objects.get(&key) else {
            return;
        };
        let LoadState::Loaded(cols) = &state.columns else {
            self.last_info = Some("Columns haven't loaded yet".into());
            return;
        };
        let Some(col) = cols.iter().find(|c| c.name == name) else {
            self.last_info = Some(format!("Column `{name}` is no longer present"));
            return;
        };
        self.column_edit_modal = Some(ColumnEditState::new_modify(key, col));
    }

    /// Build `ALTER TABLE … DROP COLUMN …` and route through the
    /// destructive-confirm modal. Type-to-confirm gate uses the column
    /// name (set by `dialog::confirm_target`).
    fn enqueue_drop_column(&mut self, key: ObjectKey, name: String) {
        let sql = column_dialog::build_drop_column_sql(&key, &name);
        self.confirm = Some(ConfirmAction {
            title: format!("Drop column `{name}` from `{}`.`{}`", key.db, key.name),
            message: format!(
                "This will permanently delete the `{name}` column and all of \
                 its data from `{}`.`{}`.",
                key.db, key.name
            ),
            sql,
            kind: PendingExec::DropColumn { key, name },
        });
        self.confirm_typed.clear();
    }

    /// Build the DROP + CREATE pair for the user's edited Source body and
    /// route the request through the destructive-confirm modal so the user
    /// gets a chance to back out.
    fn enqueue_save_source(&mut self, key: ObjectKey, body: String) {
        if !object_view::supports_source_editing(key.kind) {
            self.last_error = Some(format!(
                "Source editing is not supported for {:?}",
                key.kind
            ));
            return;
        }
        let qualified = format!(
            "`{}`.`{}`",
            key.db.replace('`', "``"),
            key.name.replace('`', "``")
        );
        let drop_keyword = match key.kind {
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::View => "VIEW",
            // Excluded by `supports_source_editing`.
            ObjectKind::Table | ObjectKind::Trigger | ObjectKind::Event => return,
        };
        let drop_sql = format!("DROP {drop_keyword} IF EXISTS {qualified}");
        let preview = format!("{drop_sql};\n\n{body}");
        let kind_label = match key.kind {
            ObjectKind::Procedure => "procedure",
            ObjectKind::Function => "function",
            ObjectKind::View => "view",
            _ => "object",
        };
        let title = format!("Replace {kind_label} `{}`.`{}`", key.db, key.name);
        // For views we could use `CREATE OR REPLACE VIEW`, but it requires
        // surgery on the body the user just edited. DROP + CREATE keeps the
        // server-side behavior identical for all three kinds at the cost of
        // a (sub-millisecond) window where the object doesn't exist.
        let message = format!(
            "DDL is not transactional in MySQL/MariaDB. If the CREATE fails \
             after the DROP succeeds, `{}`.`{}` will be left dropped until \
             you fix and re-save the body.",
            key.db, key.name
        );
        self.confirm = Some(ConfirmAction {
            title,
            message,
            sql: preview,
            kind: PendingExec::ReplaceSource {
                key,
                drop_sql: Some(drop_sql),
                create_sql: body,
            },
        });
        self.confirm_typed.clear();
    }

    /// Run `SELECT * FROM \`db\`.\`tbl\` LIMIT 1000` and route the result
    /// into the Object view's Data subtab (NOT a new dock Results tab —
    /// the grid lives inside the Object tab to avoid duplication).
    fn load_object_data(&mut self, key: ObjectKey) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let db = key.db.clone();
        let name = key.name.clone();
        let page_size = results::DEFAULT_PAGE_SIZE;
        let qualified = format!("`{}`.`{}`", db.replace('`', "``"), name.replace('`', "``"));
        let user_sql = format!("SELECT * FROM {qualified}");
        let exec_sql = format!("SELECT * FROM {qualified} LIMIT {page_size}");
        let event_key = key.clone();
        self.bridge.spawn(async move {
            let result = handle.query(exec_sql).await.map_err(|e| e.friendly());
            UiEvent::ObjectDataLoaded {
                profile,
                key: event_key,
                label: format!("{db}.{name}"),
                sql: user_sql,
                page_size,
                result,
            }
        });
    }

    /// Run when `ResultsState::push` evicts an old tab past the cap: drop
    /// the matching dock tab AND mark any Object-view Data subtab that was
    /// backed by it as `NotLoaded` so the next visit re-fetches.
    fn cleanup_evicted_result_tab(&mut self, tab_id: u64) {
        let target = self.dock.iter_all_tabs().find_map(|(path, tab)| {
            matches!(tab, DockTab::Results { tab_id: id } if *id == tab_id).then_some(path)
        });
        if let Some(path) = target {
            self.dock.remove_tab(path);
        }
        for state in self.objects.values_mut() {
            if state.data_tab_id() == Some(tab_id) {
                state.data = LoadState::NotLoaded;
            }
        }
    }

    fn is_active_profile(&self, profile: &str) -> bool {
        self.active.as_ref().map(|a| a.profile_name.as_str()) == Some(profile)
    }

    /// Open (or focus) the object inspector tab for `key`. If a tab already
    /// exists in the dock, just bring it to the front; otherwise create the
    /// backing [`ObjectViewState`] and push a new `DockTab::Object`.
    fn focus_or_open_object(&mut self, key: ObjectKey) {
        let existing = self.dock.iter_all_tabs().find_map(|(path, tab)| {
            matches!(tab, DockTab::Object { key: k } if k == &key).then_some(path)
        });
        if let Some(path) = existing {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        self.objects
            .entry(key.clone())
            .or_insert_with(|| ObjectViewState::new(key.kind, key.db.clone(), key.name.clone()));
        self.dock.push_to_focused_leaf(DockTab::Object { key });
    }

    /// Consume `pending_editor_focus`: if a close path just changed the
    /// focused dock tab to a SQL editor, request keyboard focus on its
    /// `TextEdit` so the user can keep typing without a stray click.
    fn focus_editor_after_close(&mut self, ctx: &egui::Context) {
        if !self.pending_editor_focus {
            return;
        }
        self.pending_editor_focus = false;
        let Some(node_path) = self.dock.focused_leaf() else {
            return;
        };
        let Ok(leaf) = self.dock.leaf(node_path) else {
            return;
        };
        let Some(DockTab::SqlEditor { buffer_id }) = leaf.tabs.get(leaf.active.0) else {
            return;
        };
        let id = egui::Id::new(("editor-textedit", *buffer_id));
        ctx.memory_mut(|m| m.request_focus(id));
    }

    fn open_new_dialog(&mut self) {
        self.dialog = Some(NewConnectionDialog::default());
    }

    fn run_test(&mut self, profile: ConnectionProfile, password: String) {
        let key = profile.name.clone();
        self.in_flight.insert(key.clone());
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.last_test = Some(TestOutcome::Pending);
        }
        self.bridge.spawn(async move {
            let result = test_connection(&profile, &password)
                .await
                .map_err(|e| e.friendly());
            UiEvent::TestResult {
                profile: key,
                result,
            }
        });
    }

    fn save_dialog(&mut self) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let profile = match dialog.to_profile() {
            Ok(p) => p,
            Err(msg) => {
                self.last_error = Some(msg);
                return;
            }
        };
        let password = dialog.password.clone();

        if let Err(e) = secret::store_password(&profile.keyring_account(), &password) {
            self.last_error = Some(format!("Keyring: {e}"));
            return;
        }

        if let Some(existing) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }

        if let Err(e) = self.store.save(&self.profiles) {
            self.last_error = Some(format!("Save profiles: {e}"));
            return;
        }

        self.dialog = None;
    }

    fn connect(&mut self, profile_name: &str) {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|p| p.name == profile_name)
            .cloned()
        else {
            return;
        };
        let password = match secret::fetch_password(&profile.keyring_account()) {
            Ok(Some(p)) => p,
            Ok(None) => {
                self.last_error = Some(format!("No password in keyring for '{profile_name}'"));
                return;
            }
            Err(e) => {
                self.last_error = Some(format!("Keyring: {e}"));
                return;
            }
        };

        let key = profile.name.clone();
        self.in_flight.insert(key.clone());
        let rt = crate::runtime::handle();
        self.bridge.spawn(async move {
            match build_pool(&profile, &password).await {
                Ok(pool) => {
                    let handle = DbActor::spawn(&rt, pool);
                    match handle.server_info().await {
                        Ok(info) => UiEvent::Connected {
                            profile: key,
                            handle,
                            info,
                        },
                        Err(e) => {
                            handle.shutdown();
                            UiEvent::ConnectFailed {
                                profile: key,
                                error: e.to_string(),
                            }
                        }
                    }
                }
                Err(e) => UiEvent::ConnectFailed {
                    profile: key,
                    error: e.to_string(),
                },
            }
        });
    }

    fn disconnect(&mut self) {
        if let Some(active) = self.active.take() {
            active.handle.shutdown();
        }
    }

    fn delete_profile(&mut self, name: &str) {
        if let Some(idx) = self.profiles.iter().position(|p| p.name == name) {
            let removed = self.profiles.remove(idx);
            let _ = secret::delete_password(&removed.keyring_account());
            if let Err(e) = self.store.save(&self.profiles) {
                self.last_error = Some(format!("Save profiles: {e}"));
            }
            if self.active.as_ref().is_some_and(|a| a.profile_name == name) {
                self.disconnect();
            }
        }
    }

    fn fetch_databases(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        active.schema.databases = LoadState::Loading;
        self.bridge.spawn(async move {
            let result = handle.list_databases().await.map_err(|e| e.friendly());
            UiEvent::DatabasesListed { profile, result }
        });
    }

    fn fetch_objects(&mut self, db: &str) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let db_owned = db.to_string();
        active
            .schema
            .objects
            .insert(db_owned.clone(), LoadState::Loading);
        self.bridge.spawn(async move {
            let result = handle
                .list_objects(db_owned.clone())
                .await
                .map_err(|e| e.friendly());
            UiEvent::ObjectsListed {
                profile,
                db: db_owned,
                result,
            }
        });
    }

    fn show_create(&mut self, db: String, kind: ObjectKind, name: String) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let name_clone = name.clone();
        self.bridge.spawn(async move {
            let result = handle
                .show_create(db, kind, name_clone.clone())
                .await
                .map_err(|e| e.friendly());
            UiEvent::ShowCreate {
                profile,
                name: name_clone,
                result,
            }
        });
    }

    fn execute_pending(&mut self, action: ConfirmAction) {
        // EditCell uses a different result event so we can update the cell in
        // place; route it separately.
        if let PendingExec::EditCell(req) = action.kind {
            self.run_edit(req);
            return;
        }
        // ReplaceSource ships two statements through `replace_routine`,
        // bypassing the client-side splitter.
        if let PendingExec::ReplaceSource {
            key,
            drop_sql,
            create_sql,
        } = action.kind
        {
            self.run_replace_source(key, drop_sql, create_sql);
            return;
        }

        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let exec_kind = match &action.kind {
            PendingExec::DropObject { db, .. } | PendingExec::Truncate { db, .. } => {
                ExecKind::AlteredDb(db.clone())
            }
            PendingExec::DropDatabase { .. } => ExecKind::DroppedDatabase,
            PendingExec::InsertRow { tab_id, .. } => ExecKind::InsertedRow { tab_id: *tab_id },
            PendingExec::DeleteRows { tab_id, rows } => ExecKind::DeletedRows {
                tab_id: *tab_id,
                rows: rows.clone(),
            },
            PendingExec::BulkUpdate {
                tab_id,
                rows,
                col_idx,
                new_value,
            } => ExecKind::BulkUpdated {
                tab_id: *tab_id,
                rows: rows.clone(),
                col_idx: *col_idx,
                new_value: new_value.clone(),
            },
            PendingExec::AlterColumn { key } | PendingExec::DropColumn { key, .. } => {
                ExecKind::AlteredColumns(key.clone())
            }
            PendingExec::EditCell(_) | PendingExec::ReplaceSource { .. } => unreachable!(),
        };
        let sql = action.sql.clone();
        self.bridge.spawn(async move {
            let result = handle.execute(sql).await.map_err(|e| e.friendly());
            UiEvent::ExecResult {
                profile,
                kind: exec_kind,
                result,
            }
        });
    }

    fn run_replace_source(&mut self, key: ObjectKey, drop_sql: Option<String>, create_sql: String) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let event_key = key.clone();
        self.bridge.spawn(async move {
            let result = handle
                .replace_routine(drop_sql, create_sql)
                .await
                .map_err(|e| e.friendly());
            UiEvent::ExecResult {
                profile,
                kind: ExecKind::ReplaceSource(event_key),
                result,
            }
        });
    }

    fn apply_sidebar(&mut self, ctx: &egui::Context, actions: Vec<SidebarAction>) {
        for action in actions {
            match action {
                SidebarAction::NewConnection => self.open_new_dialog(),
                SidebarAction::Connect(name) => self.connect(&name),
                SidebarAction::Disconnect => self.disconnect(),
                SidebarAction::DeleteProfile(name) => self.delete_profile(&name),
                SidebarAction::RefreshDatabases => self.fetch_databases(),
                SidebarAction::RefreshDatabase(db) => self.fetch_objects(&db),
                SidebarAction::CopyText(text) => {
                    ctx.copy_text(text.clone());
                    self.last_info = Some(format!("Copied: {text}"));
                }
                SidebarAction::ShowCreate { db, kind, name } => {
                    self.show_create(db, kind, name);
                }
                SidebarAction::OpenObject { db, kind, name } => {
                    self.focus_or_open_object(ObjectKey::new(kind, db, name));
                }
                SidebarAction::Confirm(action) => {
                    self.confirm = Some(action);
                    self.confirm_typed.clear();
                }
            }
        }
    }

    fn render_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::containers::menu::MenuBar::new().ui(ui, |ui| {
            egui::containers::menu::MenuButton::new("File").ui(ui, |ui| {
                if ui.button("New connection…").clicked() {
                    self.open_new_dialog();
                    ui.close();
                }
                ui.separator();
                if ui.button("New SQL tab").clicked() {
                    self.open_new_editor_tab();
                    ui.close();
                }
                if ui.button("Close current tab").clicked() {
                    self.close_focused_dock_tab();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            egui::containers::menu::MenuButton::new("Edit").ui(ui, |ui| {
                if ui.button("History…").clicked() {
                    self.open_history();
                    ui.close();
                }
            });
            egui::containers::menu::MenuButton::new("View").ui(ui, |ui| {
                let enabled = self.active.is_some();
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh schema"))
                    .clicked()
                {
                    self.fetch_databases();
                    ui.close();
                }
                ui.separator();
                ui.label(egui::RichText::new("Theme").weak().small());
                let mut changed = false;
                for (label, value) in [
                    ("Follow system", ThemeChoice::System),
                    ("Light", ThemeChoice::Light),
                    ("Dark", ThemeChoice::Dark),
                ] {
                    if ui
                        .selectable_value(&mut self.settings.theme, value, label)
                        .changed()
                    {
                        changed = true;
                    }
                }
                if changed {
                    apply_theme(ctx, self.settings.theme);
                    if let Err(e) = self.settings_store.save(&self.settings) {
                        tracing::warn!(error = %e, "failed to persist settings");
                    }
                }
                ui.separator();
                if ui.button("Close all SQL tabs").clicked() {
                    self.close_all_editor_tabs();
                    ui.close();
                }
                if ui.button("Close all results tabs").clicked() {
                    self.close_all_results_tabs();
                    ui.close();
                }
            });
            egui::containers::menu::MenuButton::new("Help").ui(ui, |ui| {
                if ui.button("About RySQL").clicked() {
                    ui.close();
                }
            });
        });
    }

    fn open_history(&mut self) {
        match self.history_store.load() {
            Ok(entries) => {
                self.history_view.entries = entries;
                self.history_view.open = true;
            }
            Err(e) => {
                self.last_error = Some(format!("History: {e}"));
            }
        }
    }

    fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        let mut cancel = false;
        ui.horizontal(|ui| {
            match &self.active {
                Some(a) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                        format!("● {}", a.profile_name),
                    );
                    ui.separator();
                    ui.label(format!("Server: {}", a.info.version));
                }
                None => {
                    ui.label("Disconnected");
                }
            }
            if self.in_flight_query.is_some() {
                ui.separator();
                ui.spinner();
                ui.label(
                    egui::RichText::new("Running…")
                        .color(egui::Color32::from_rgb(0x8a, 0xb4, 0xf8)),
                );
                if ui.small_button("Cancel").clicked() {
                    cancel = true;
                }
            } else if let Some(err) = &self.last_error {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), err);
            } else if let Some(info) = &self.last_info {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(0x8a, 0xb4, 0xf8), info);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
            });
        });
        if cancel {
            self.cancel_in_flight();
        }
    }

    fn render_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog_state) = self.dialog.as_mut() else {
            return;
        };
        let action = dialog::show(ctx, dialog_state);
        match action {
            DialogAction::None => {}
            DialogAction::Cancel => self.dialog = None,
            DialogAction::Test => {
                let dialog = self.dialog.as_ref().unwrap();
                match dialog.to_profile() {
                    Ok(profile) => {
                        let password = dialog.password.clone();
                        self.run_test(profile, password);
                    }
                    Err(msg) => {
                        if let Some(d) = self.dialog.as_mut() {
                            d.last_test = Some(TestOutcome::Err(msg));
                        }
                    }
                }
            }
            DialogAction::Save => self.save_dialog(),
        }
    }

    fn render_confirm(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm.as_ref().cloned() else {
            return;
        };
        let choice = dialog::show_confirm(ctx, &action, &mut self.confirm_typed);
        match choice {
            ConfirmChoice::None => {}
            ConfirmChoice::Cancel => {
                self.confirm = None;
                self.confirm_typed.clear();
            }
            ConfirmChoice::Confirm => {
                self.confirm = None;
                self.confirm_typed.clear();
                self.execute_pending(action);
            }
        }
    }
}

fn sql_label(sql: &str) -> String {
    let one_line: String = sql.split('\n').next().unwrap_or("").trim().to_string();
    if one_line.len() <= 48 {
        one_line
    } else {
        format!("{}…", &one_line.chars().take(47).collect::<String>())
    }
}

impl eframe::App for RysqlApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);

        egui::Panel::top("menu_bar").show_inside(ui, |ui| self.render_menu(ui, &ctx));

        egui::Panel::bottom("status_bar").show_inside(ui, |ui| self.render_status_bar(ui));

        egui::Panel::left("sidebar")
            .default_size(280.0)
            // Defensive size bounds. With the sidebar's filter input now
            // requesting only the currently-available width (not INFINITY),
            // the panel won't auto-grow with content — these only matter
            // when the user is dragging the divider.
            .min_size(200.0)
            .max_size(600.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                let input = SidebarInput {
                    profiles: &self.profiles,
                    active: self.active.as_ref(),
                    in_flight: &self.in_flight,
                    filter: &mut self.schema_filter,
                };
                let actions = sidebar::render(ui, input);
                self.apply_sidebar(&ctx, actions);
            });

        let shortcut_actions =
            editor::handle_shortcuts(&ctx, self.confirm.is_none() && self.dialog.is_none());

        let schema_names = self.collect_schema_names();
        let mut dock_actions: Vec<DockAction> = Vec::new();
        let mut dock_results_actions: Vec<ResultsAction> = Vec::new();
        let mut request_new_editor_tab = false;
        let dock_empty = self.dock.iter_all_tabs().next().is_none();
        egui::CentralPanel::default()
            .frame(egui::Frame::default().inner_margin(egui::Margin::same(0)))
            .show_inside(ui, |ui| {
                if dock_empty {
                    render_empty_dock_placeholder(ui, &mut request_new_editor_tab);
                    return;
                }
                let mut viewer = AppViewer::new(
                    &mut self.editor,
                    &mut self.results,
                    &mut self.objects,
                    &mut self.source_highlighter,
                    &schema_names,
                );
                DockArea::new(&mut self.dock)
                    .style(Style::from_egui(ui.style().as_ref()))
                    .show_close_buttons(true)
                    .show_add_buttons(false)
                    .draggable_tabs(true)
                    .show_inside(ui, &mut viewer);
                dock_actions = viewer.actions;
                dock_results_actions = viewer.results_actions;
            });
        if request_new_editor_tab {
            self.open_new_editor_tab();
        }
        self.apply_dock_actions(dock_actions);
        self.apply_results_actions(&ctx, dock_results_actions);
        self.apply_editor_actions(shortcut_actions);

        // Ctrl+Del on the focused Results / Object-Data tab → bulk delete.
        // Captured here so any open modal (which renders later in the
        // frame) doesn't intercept the key.
        if self.confirm.is_none() && self.dialog.is_none() {
            let pressed =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Delete));
            if pressed {
                self.delete_focused_selection();
            }
        }

        self.render_dialog(&ctx);
        self.render_confirm(&ctx);

        let viewer_actions = results::render_blob_viewer(&ctx, &mut self.results.blob_viewer);
        self.apply_results_actions(&ctx, viewer_actions);

        self.render_edit_modal(&ctx);
        self.render_insert_modal(&ctx);
        self.render_bulk_update_modal(&ctx);
        self.render_column_edit_modal(&ctx);
        self.render_history(&ctx);

        // Last so it takes effect on the *next* frame's editor render.
        self.focus_editor_after_close(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
    }
}

/// Friendly placeholder for the central panel when every tab has been
/// closed. Offers an action button and a short hint pointing at the
/// schema sidebar / `Ctrl+T` shortcut. The bool flag is set when the
/// user clicks the button so the caller can dispatch
/// `open_new_editor_tab` outside the closure.
fn render_empty_dock_placeholder(ui: &mut egui::Ui, request_new_tab: &mut bool) {
    let avail_h = ui.available_height();
    ui.vertical_centered(|ui| {
        ui.add_space(avail_h * 0.32);
        ui.label(egui::RichText::new("No tabs open").size(24.0).weak());
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Double-click an object in the schema sidebar to inspect it,")
                .weak(),
        );
        ui.label(egui::RichText::new("or open a new SQL tab to start a query.").weak());
        ui.add_space(16.0);
        if ui
            .button(egui::RichText::new("+ New SQL tab").strong())
            .on_hover_text("Ctrl+T · File → New SQL tab")
            .clicked()
        {
            *request_new_tab = true;
        }
    });
}

fn apply_theme(ctx: &egui::Context, choice: ThemeChoice) {
    let pref = match choice {
        ThemeChoice::Dark => egui::ThemePreference::Dark,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::System => egui::ThemePreference::System,
    };
    ctx.set_theme(pref);
}

impl RysqlApp {
    fn render_history(&mut self, ctx: &egui::Context) {
        let action = history_view::render(ctx, &mut self.history_view);
        match action {
            HistoryAction::None => {}
            HistoryAction::Close => {
                self.history_view.open = false;
            }
            HistoryAction::LoadIntoEditor(sql) => {
                self.editor.new_buffer();
                if let Some(buf) = self.editor.buffers.get_mut(self.editor.active) {
                    buf.text = sql;
                    buf.dirty = true;
                }
                self.history_view.open = false;
            }
            HistoryAction::Clear => {
                if let Err(e) = self.history_store.clear() {
                    self.last_error = Some(format!("Clear history: {e}"));
                } else {
                    self.history_view.entries.clear();
                    self.last_info = Some("History cleared".into());
                }
            }
        }
    }

    fn render_edit_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.results.edit_modal.take() else {
            return;
        };
        let choice = results::render_edit_modal(ctx, &mut state);
        match choice {
            EditDialogChoice::None => {
                self.results.edit_modal = Some(state);
            }
            EditDialogChoice::Cancel => {
                // dropped
            }
            EditDialogChoice::Submit => {
                let tab_idx = match self.results.find_by_id(state.tab_id) {
                    Some(i) => i,
                    None => return,
                };
                let tab = &self.results.tabs[tab_idx];
                let Some(target) = tab.editable.as_ref() else {
                    self.last_error = Some("Cell no longer editable".into());
                    return;
                };
                let Some(row) = tab.result.rows.get(state.row) else {
                    self.last_error = Some("Row no longer present".into());
                    return;
                };
                match results::build_update_sql(
                    target,
                    &tab.result.columns,
                    row,
                    state.col,
                    &state.new_value,
                ) {
                    Ok(sql) => {
                        self.apply_results_actions(
                            ctx,
                            vec![ResultsAction::ApplyEdit(EditRequest {
                                tab_id: state.tab_id,
                                row: state.row,
                                col: state.col,
                                preview: sql.clone(),
                                sql,
                                new_value: state.new_value.clone(),
                            })],
                        );
                    }
                    Err(e) => {
                        self.last_error = Some(format!("Cannot build UPDATE: {e}"));
                    }
                }
            }
        }
    }

    fn render_insert_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.results.insert_modal.take() else {
            return;
        };
        let choice = results::render_insert_modal(ctx, &mut state);
        match choice {
            InsertChoice::None => {
                self.results.insert_modal = Some(state);
            }
            InsertChoice::Cancel => {
                // dropped
            }
            InsertChoice::Submit { sql } => {
                self.confirm = Some(ConfirmAction {
                    title: format!("Insert into `{}`.`{}`", state.target.db, state.target.table),
                    message: "Apply this INSERT? The result tab will refresh \
                              afterwards so server-side defaults / auto_increment \
                              values are visible."
                        .into(),
                    sql,
                    kind: PendingExec::InsertRow {
                        tab_id: state.tab_id,
                    },
                });
                self.confirm_typed.clear();
            }
        }
    }

    fn render_bulk_update_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.results.bulk_update_modal.take() else {
            return;
        };
        // The preview builder reads from the live `tab.result.rows`; if the
        // tab vanished while the modal was open we just close.
        let rows = match self.results.find_by_id(state.tab_id) {
            Some(idx) => self.results.tabs[idx].result.rows.clone(),
            None => return,
        };
        let choice = results::render_bulk_update_modal(ctx, &mut state, &rows);
        match choice {
            BulkUpdateChoice::None => {
                self.results.bulk_update_modal = Some(state);
            }
            BulkUpdateChoice::Cancel => {
                // dropped
            }
            BulkUpdateChoice::Submit {
                sql,
                col_idx,
                new_value,
            } => {
                let count = state.selected_rows.len();
                self.confirm = Some(ConfirmAction {
                    title: format!(
                        "Update {count} row(s) in `{}`.`{}`",
                        state.target.db, state.target.table
                    ),
                    message: format!(
                        "Apply this UPDATE to {count} row(s)? Selected cells \
                         will reflect the new value immediately on success."
                    ),
                    sql,
                    kind: PendingExec::BulkUpdate {
                        tab_id: state.tab_id,
                        rows: state.selected_rows.clone(),
                        col_idx,
                        new_value,
                    },
                });
                self.confirm_typed.clear();
            }
        }
    }

    fn render_column_edit_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.column_edit_modal.take() else {
            return;
        };
        let choice = column_dialog::render_column_edit_modal(ctx, &mut state);
        match choice {
            ColumnEditChoice::None => {
                self.column_edit_modal = Some(state);
            }
            ColumnEditChoice::Cancel => {
                // dropped
            }
            ColumnEditChoice::Submit { sql } => {
                let title = match &state.mode {
                    ColumnEditMode::Add => format!(
                        "Add column `{}` to `{}`.`{}`",
                        state.name.trim(),
                        state.key.db,
                        state.key.name
                    ),
                    ColumnEditMode::Modify { old_name } => format!(
                        "Modify column `{old_name}` of `{}`.`{}`",
                        state.key.db, state.key.name,
                    ),
                };
                self.confirm = Some(ConfirmAction {
                    title,
                    message: "Apply this ALTER TABLE? The Structure subtab \
                              and any open Data subtab will refresh on success."
                        .into(),
                    sql,
                    kind: PendingExec::AlterColumn { key: state.key },
                });
                self.confirm_typed.clear();
            }
        }
    }
}
