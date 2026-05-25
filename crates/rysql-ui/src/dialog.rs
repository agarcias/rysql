//! Modal dialogs: "New connection" and destructive-action confirmation.

use eframe::egui;
use rysql_core::{ConnectionProfile, TlsMode};

use crate::state::ConfirmAction;

#[derive(Debug, Clone)]
pub struct NewConnectionDialog {
    pub name: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub database: String,
    pub socket: String,
    pub tls: TlsMode,
    pub last_test: Option<TestOutcome>,
}

#[derive(Debug, Clone)]
pub enum TestOutcome {
    Pending,
    Ok(String),
    Err(String),
}

impl Default for NewConnectionDialog {
    fn default() -> Self {
        let d = ConnectionProfile::default();
        Self {
            name: d.name,
            host: d.host,
            port: d.port.to_string(),
            user: d.user,
            password: String::new(),
            database: String::new(),
            socket: String::new(),
            tls: d.tls,
            last_test: None,
        }
    }
}

impl NewConnectionDialog {
    pub fn to_profile(&self) -> Result<ConnectionProfile, String> {
        let port: u16 = self
            .port
            .parse()
            .map_err(|_| "Port must be 1..=65535".to_string())?;
        if self.name.trim().is_empty() {
            return Err("Name is required".into());
        }
        if self.host.trim().is_empty() {
            return Err("Host is required".into());
        }
        if self.user.trim().is_empty() {
            return Err("User is required".into());
        }
        Ok(ConnectionProfile {
            name: self.name.trim().to_string(),
            host: self.host.trim().to_string(),
            port,
            user: self.user.trim().to_string(),
            database: opt_str(&self.database),
            socket: opt_str(&self.socket),
            tls: self.tls,
        })
    }
}

fn opt_str(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

pub enum DialogAction {
    None,
    Test,
    Save,
    Cancel,
}

pub fn show(ctx: &egui::Context, state: &mut NewConnectionDialog) -> DialogAction {
    let mut action = DialogAction::None;
    egui::Modal::new(egui::Id::new("new-connection-modal")).show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.heading("New connection");
        ui.add_space(8.0);

        egui::Grid::new("new-conn-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(260.0));
                ui.end_row();

                ui.label("Host");
                ui.add(egui::TextEdit::singleline(&mut state.host).desired_width(260.0));
                ui.end_row();

                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut state.port).desired_width(80.0));
                ui.end_row();

                ui.label("User");
                ui.add(egui::TextEdit::singleline(&mut state.user).desired_width(260.0));
                ui.end_row();

                ui.label("Password");
                ui.add(
                    egui::TextEdit::singleline(&mut state.password)
                        .password(true)
                        .desired_width(260.0),
                );
                ui.end_row();

                ui.label("Database");
                ui.add(
                    egui::TextEdit::singleline(&mut state.database)
                        .hint_text("optional")
                        .desired_width(260.0),
                );
                ui.end_row();

                ui.label("Unix socket");
                ui.add(
                    egui::TextEdit::singleline(&mut state.socket)
                        .hint_text("optional, overrides host/port")
                        .desired_width(260.0),
                );
                ui.end_row();

                ui.label("TLS");
                egui::ComboBox::from_id_salt("tls")
                    .selected_text(format!("{:?}", state.tls))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.tls, TlsMode::Disabled, "Disabled");
                        ui.selectable_value(&mut state.tls, TlsMode::Preferred, "Preferred");
                        ui.selectable_value(&mut state.tls, TlsMode::Required, "Required");
                    });
                ui.end_row();
            });

        ui.add_space(8.0);
        if let Some(outcome) = &state.last_test {
            match outcome {
                TestOutcome::Pending => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Testing connection…");
                    });
                }
                TestOutcome::Ok(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(0x4c, 0xaf, 0x50), msg);
                }
                TestOutcome::Err(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), msg);
                }
            }
            ui.add_space(4.0);
        }

        ui.separator();
        ui.horizontal(|ui| {
            let testing = matches!(state.last_test, Some(TestOutcome::Pending));
            if ui
                .add_enabled(!testing, egui::Button::new("Test"))
                .clicked()
            {
                action = DialogAction::Test;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Save").clicked() {
                    action = DialogAction::Save;
                }
                if ui.button("Cancel").clicked() {
                    action = DialogAction::Cancel;
                }
            });
        });
    });
    action
}

#[derive(Debug, Clone, Copy)]
pub enum ConfirmChoice {
    None,
    Confirm,
    Cancel,
}

/// Renders the destructive-action confirmation modal. The caller passes the
/// pending action; the user must type the object name to enable Confirm.
pub fn show_confirm(
    ctx: &egui::Context,
    action: &ConfirmAction,
    typed: &mut String,
) -> ConfirmChoice {
    let mut choice = ConfirmChoice::None;
    egui::Modal::new(egui::Id::new("confirm-modal")).show(ctx, |ui| {
        // Wider than the older 440 default so multi-statement DDL previews
        // (DROP + CREATE for procedures, etc.) don't soft-wrap aggressively.
        ui.set_min_width(992.0);
        ui.heading(&action.title);
        ui.add_space(8.0);
        ui.label(&action.message);
        ui.add_space(8.0);

        ui.label(egui::RichText::new("SQL to be executed:").weak().small());
        let mut sql = action.sql.clone();
        // Cap the SQL preview's height: long procedure bodies would push
        // the Cancel/Confirm row off-screen otherwise. The ScrollArea
        // shrinks vertically when the content is short, so simple DROP
        // statements still render compact.
        egui::ScrollArea::vertical()
            .id_salt("confirm-sql-scroll")
            .max_height(512.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut sql)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .interactive(false)
                        .font(egui::TextStyle::Monospace),
                );
            });
        ui.add_space(8.0);

        let enabled = match confirm_target(action) {
            Some(target) => {
                ui.label(format!(
                    "Type the {} name to confirm:",
                    confirm_target_label(action)
                ));
                ui.label(egui::RichText::new(&target).strong().monospace());
                ui.add(egui::TextEdit::singleline(typed).desired_width(f32::INFINITY));
                ui.add_space(8.0);
                typed.trim() == target
            }
            None => true,
        };

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                choice = ConfirmChoice::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = egui::Button::new(
                    egui::RichText::new("Confirm").color(egui::Color32::from_rgb(0xe5, 0x73, 0x73)),
                );
                if ui.add_enabled(enabled, btn).clicked() {
                    choice = ConfirmChoice::Confirm;
                }
            });
        });
    });
    choice
}

/// `Some(target)` when the user must type the target name to confirm; `None`
/// when a plain Confirm button suffices (non-destructive edits).
fn confirm_target(action: &ConfirmAction) -> Option<String> {
    use crate::state::PendingExec;
    match &action.kind {
        PendingExec::DropObject { name, .. } => Some(name.clone()),
        PendingExec::Truncate { name, .. } => Some(name.clone()),
        PendingExec::DropDatabase { db } => Some(db.clone()),
        PendingExec::EditCell(_) => None,
        // ReplaceSource is destructive (drops the routine) but the body the
        // user is about to submit is already visible in the SQL preview; a
        // type-to-confirm gate on top of that is overkill.
        PendingExec::ReplaceSource { .. } => None,
        // INSERT is non-destructive (it only adds a row); same reasoning —
        // the SQL preview is already in front of the user.
        PendingExec::InsertRow { .. } => None,
    }
}

fn confirm_target_label(action: &ConfirmAction) -> &'static str {
    use crate::state::PendingExec;
    match action.kind {
        PendingExec::DropObject { .. } => "object",
        PendingExec::Truncate { .. } => "table",
        PendingExec::DropDatabase { .. } => "database",
        PendingExec::EditCell(_) => "cell",
        PendingExec::ReplaceSource { .. } => "routine",
        PendingExec::InsertRow { .. } => "row",
    }
}
