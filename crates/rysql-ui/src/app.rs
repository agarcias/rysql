use std::collections::HashSet;

use eframe::egui;
use rysql_core::{
    secret, store::ProfileStore, AppSettings, ConnectionProfile, HistoryEntry, HistoryStore,
    SettingsStore, ThemeChoice,
};
use rysql_db::{build_pool, test_connection, DbActor, ObjectKind};
use tokio::task::AbortHandle;

use crate::bridge::{Bridge, ExecKind, UiEvent};
use crate::dialog::{self, ConfirmChoice, DialogAction, NewConnectionDialog, TestOutcome};
use crate::editor::{self, EditorAction, EditorState};
use crate::history_view::{self, HistoryAction, HistoryView};
#[allow(unused_imports)]
use crate::results::EditRequest;
use crate::results::{
    self, EditCellState, EditDialogChoice, ExportFormat, ResultTab, ResultsAction, ResultsState,
};
use crate::sidebar::{self, SidebarAction, SidebarInput};
use crate::state::{ActiveConnection, ConfirmAction, LoadState, PendingExec, SchemaState};

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
    results: ResultsState,
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
            editor: EditorState::default(),
            results: ResultsState::default(),
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
                        self.results.push(tab);
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
        }
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
                ResultsAction::SelectTab(i) => {
                    if i < self.results.tabs.len() {
                        self.results.active = i;
                    }
                }
                ResultsAction::CloseTab(i) => self.results.close(i),
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
                ResultsAction::Export { tab_idx, format } => {
                    if let Some(tab) = self.results.tabs.get(tab_idx) {
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
            }
        }
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
                EditorAction::NewTab => {
                    self.editor.new_buffer();
                }
                EditorAction::SelectTab(idx) => {
                    if idx < self.editor.buffers.len() {
                        self.editor.active = idx;
                    }
                }
                EditorAction::CloseTab(mut idx) => {
                    if idx == usize::MAX {
                        idx = self.editor.active;
                    }
                    self.editor.close_buffer(idx);
                }
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
            PendingExec::EditCell(_) => unreachable!(),
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
            .resizable(true)
            .show_inside(ui, |ui| {
                let input = SidebarInput {
                    profiles: &self.profiles,
                    active: self.active.as_ref(),
                    in_flight: &self.in_flight,
                };
                let actions = sidebar::render(ui, input);
                self.apply_sidebar(&ctx, actions);
            });

        let shortcut_actions =
            editor::handle_shortcuts(&ctx, self.confirm.is_none() && self.dialog.is_none());

        let mut results_actions = Vec::new();
        if !self.results.is_empty() {
            egui::Panel::bottom("results-pane")
                .resizable(true)
                .default_size(320.0)
                .min_size(140.0)
                .show_inside(ui, |ui| {
                    results_actions = results::render(ui, &mut self.results);
                });
        }
        self.apply_results_actions(&ctx, results_actions);

        let schema_names = self.collect_schema_names();
        let mut editor_actions = Vec::new();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let ec = editor::EditorContext {
                schema_names: &schema_names,
            };
            editor_actions = editor::render(ui, &mut self.editor, ec);
        });
        editor_actions.extend(shortcut_actions);
        self.apply_editor_actions(editor_actions);

        self.render_dialog(&ctx);
        self.render_confirm(&ctx);

        let viewer_actions = results::render_blob_viewer(&ctx, &mut self.results.blob_viewer);
        self.apply_results_actions(&ctx, viewer_actions);

        self.render_edit_modal(&ctx);
        self.render_history(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
    }
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
}
