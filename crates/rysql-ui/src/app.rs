use std::collections::HashSet;

use eframe::egui;
use rysql_core::{secret, store::ProfileStore, ConnectionProfile};
use rysql_db::{build_pool, test_connection, DbActor, DbHandle, ServerInfo};

use crate::bridge::{Bridge, UiEvent};
use crate::dialog::{self, DialogAction, NewConnectionDialog, TestOutcome};

pub struct RysqlApp {
    bridge: Bridge,
    store: ProfileStore,
    profiles: Vec<ConnectionProfile>,
    dialog: Option<NewConnectionDialog>,
    in_flight: HashSet<String>,
    active: Option<ActiveConnection>,
    last_error: Option<String>,
}

struct ActiveConnection {
    profile_name: String,
    handle: DbHandle,
    info: ServerInfo,
}

impl RysqlApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
        }
    }

    fn handle_events(&mut self) {
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
                        profile_name: profile,
                        handle,
                        info,
                    }) {
                        prev.handle.shutdown();
                    }
                    self.last_error = None;
                }
                UiEvent::ConnectFailed { profile, error } => {
                    self.in_flight.remove(&profile);
                    self.last_error = Some(format!("{profile}: {error}"));
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
            egui::containers::menu::MenuButton::new("View").ui(ui, |_ui| {});
            egui::containers::menu::MenuButton::new("Help").ui(ui, |ui| {
                if ui.button("About RySQL").clicked() {
                    ui.close();
                }
            });
        });
    }

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Connections");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+").on_hover_text("New connection").clicked() {
                    self.open_new_dialog();
                }
            });
        });
        ui.separator();

        let active_name = self.active.as_ref().map(|a| a.profile_name.clone());
        let mut to_connect: Option<String> = None;
        let mut to_delete: Option<String> = None;
        let mut to_disconnect = false;

        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.profiles.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label("No connections yet.");
                    if ui.button("New connection…").clicked() {
                        to_connect = None;
                        self.dialog = Some(NewConnectionDialog::default());
                    }
                });
                return;
            }

            for profile in &self.profiles {
                let is_active = active_name.as_deref() == Some(profile.name.as_str());
                let is_busy = self.in_flight.contains(&profile.name);

                let label = if is_active {
                    format!("● {}", profile.name)
                } else {
                    format!("○ {}", profile.name)
                };

                ui.horizontal(|ui| {
                    let resp = ui.selectable_label(is_active, label);
                    if resp.double_clicked() && !is_active && !is_busy {
                        to_connect = Some(profile.name.clone());
                    }
                    resp.context_menu(|ui| {
                        if !is_active
                            && ui
                                .add_enabled(!is_busy, egui::Button::new("Connect"))
                                .clicked()
                        {
                            to_connect = Some(profile.name.clone());
                            ui.close();
                        }
                        if is_active && ui.button("Disconnect").clicked() {
                            to_disconnect = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            to_delete = Some(profile.name.clone());
                            ui.close();
                        }
                    });
                    if is_busy {
                        ui.spinner();
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "{}@{}:{}{}",
                        profile.user,
                        profile.host,
                        profile.port,
                        profile
                            .database
                            .as_deref()
                            .map(|d| format!("/{d}"))
                            .unwrap_or_default()
                    ))
                    .weak()
                    .small(),
                );
                ui.add_space(4.0);
            }
        });

        if to_disconnect {
            self.disconnect();
        }
        if let Some(name) = to_connect {
            self.connect(&name);
        }
        if let Some(name) = to_delete {
            self.delete_profile(&name);
        }
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
}

impl eframe::App for RysqlApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_events();
        let ctx = ui.ctx().clone();

        egui::Panel::top("menu_bar").show_inside(ui, |ui| self.render_menu(ui, &ctx));

        egui::Panel::bottom("status_bar").show_inside(ui, |ui| self.render_status_bar(ui));

        egui::Panel::left("sidebar")
            .default_size(240.0)
            .resizable(true)
            .show_inside(ui, |ui| self.render_sidebar(ui));

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading("RySQL");
                ui.label("Pick a connection on the left, or create a new one.");
            });
        });

        self.render_dialog(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
    }
}
