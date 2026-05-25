use std::collections::HashSet;

use eframe::egui;
use rysql_core::{secret, store::ProfileStore, ConnectionProfile};
use rysql_db::{build_pool, test_connection, DbActor, ObjectKind};

use crate::bridge::{Bridge, ExecKind, UiEvent};
use crate::dialog::{self, ConfirmChoice, DialogAction, NewConnectionDialog, TestOutcome};
use crate::editor::{self, EditorAction, EditorState};
use crate::results::{self, ResultTab, ResultsAction, ResultsState};
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
}

impl RysqlApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_fonts(crate::fonts::definitions());
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let bridge = Bridge::new(crate::runtime::handle(), cc.egui_ctx.clone());
        let store = ProfileStore::locate().expect("locate config dir");
        let profiles = match store.load() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load profiles, starting empty");
                Vec::new()
            }
        };

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
                    match result {
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
                    result,
                } => {
                    if self.active.as_ref().map(|a| a.profile_name.as_str()) != Some(&profile) {
                        continue;
                    }
                    match result {
                        Ok(qr) => {
                            self.last_error = None;
                            self.last_info = Some(format!(
                                "{} row(s) · {} col(s) · {:.1?}",
                                qr.rows.len(),
                                qr.columns.len(),
                                qr.elapsed
                            ));
                            self.results.push(ResultTab::new(label, qr));
                        }
                        Err(e) => {
                            self.last_error = Some(format!("Query failed: {e}"));
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
        let profile = active.profile_name.clone();
        let handle = active.handle.clone();
        let label = sql_label(&sql);
        if rysql_sql::is_query_returning_rows(&sql) {
            self.bridge.spawn(async move {
                let result = handle.query(sql).await.map_err(|e| e.to_string());
                UiEvent::QueryResult {
                    profile,
                    label,
                    result,
                }
            });
        } else {
            self.bridge.spawn(async move {
                let result = handle.execute(sql).await.map_err(|e| e.to_string());
                UiEvent::ExecResult {
                    profile,
                    kind: ExecKind::Adhoc,
                    result,
                }
            });
        }
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
                .map_err(|e| e.to_string());
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
            let result = handle.list_databases().await.map_err(|e| e.to_string());
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
                .map_err(|e| e.to_string());
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
                .map_err(|e| e.to_string());
            UiEvent::ShowCreate {
                profile,
                name: name_clone,
                result,
            }
        });
    }

    fn execute_pending(&mut self, action: ConfirmAction) {
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
        };
        let sql = action.sql.clone();
        self.bridge.spawn(async move {
            let result = handle.execute(sql).await.map_err(|e| e.to_string());
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
            egui::containers::menu::MenuButton::new("Edit").ui(ui, |_ui| {});
            egui::containers::menu::MenuButton::new("View").ui(ui, |ui| {
                let enabled = self.active.is_some();
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh schema"))
                    .clicked()
                {
                    self.fetch_databases();
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

    fn render_status_bar(&self, ui: &mut egui::Ui) {
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
            if let Some(err) = &self.last_error {
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
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
    }
}
