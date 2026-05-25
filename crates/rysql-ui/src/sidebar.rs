//! Sidebar: connections list + schema tree of the active connection.

use eframe::egui;
use rysql_core::ConnectionProfile;
use rysql_db::ObjectKind;

use crate::state::{ActiveConnection, ConfirmAction, LoadState, PendingExec};

pub enum SidebarAction {
    NewConnection,
    Connect(String),
    Disconnect,
    DeleteProfile(String),
    RefreshDatabases,
    RefreshDatabase(String),
    CopyText(String),
    ShowCreate {
        db: String,
        kind: ObjectKind,
        name: String,
    },
    Confirm(ConfirmAction),
}

pub struct SidebarInput<'a> {
    pub profiles: &'a [ConnectionProfile],
    pub active: Option<&'a ActiveConnection>,
    pub in_flight: &'a std::collections::HashSet<String>,
}

pub fn render(ui: &mut egui::Ui, input: SidebarInput<'_>) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    egui::ScrollArea::vertical().show(ui, |ui| {
        connections_section(ui, &input, &mut actions);
        ui.add_space(8.0);
        if let Some(active) = input.active {
            ui.separator();
            schema_section(ui, active, &mut actions);
        }
    });
    actions
}

fn connections_section(
    ui: &mut egui::Ui,
    input: &SidebarInput<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    ui.horizontal(|ui| {
        ui.heading("Connections");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("+").on_hover_text("New connection").clicked() {
                actions.push(SidebarAction::NewConnection);
            }
        });
    });
    ui.separator();

    if input.profiles.is_empty() {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.label("No connections yet.");
            if ui.button("New connection…").clicked() {
                actions.push(SidebarAction::NewConnection);
            }
        });
        return;
    }

    let active_name = input.active.map(|a| a.profile_name.as_str());

    for profile in input.profiles {
        let is_active = active_name == Some(profile.name.as_str());
        let is_busy = input.in_flight.contains(&profile.name);

        let label = if is_active {
            format!("● {}", profile.name)
        } else {
            format!("○ {}", profile.name)
        };

        ui.horizontal(|ui| {
            let resp = ui.selectable_label(is_active, label);
            if resp.double_clicked() && !is_active && !is_busy {
                actions.push(SidebarAction::Connect(profile.name.clone()));
            }
            resp.context_menu(|ui| {
                if !is_active
                    && ui
                        .add_enabled(!is_busy, egui::Button::new("Connect"))
                        .clicked()
                {
                    actions.push(SidebarAction::Connect(profile.name.clone()));
                    ui.close();
                }
                if is_active && ui.button("Disconnect").clicked() {
                    actions.push(SidebarAction::Disconnect);
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    actions.push(SidebarAction::DeleteProfile(profile.name.clone()));
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
}

fn schema_section(ui: &mut egui::Ui, active: &ActiveConnection, actions: &mut Vec<SidebarAction>) {
    ui.horizontal(|ui| {
        ui.heading("Schema");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("↻")
                .on_hover_text("Refresh database list")
                .clicked()
            {
                actions.push(SidebarAction::RefreshDatabases);
            }
        });
    });
    ui.label(
        egui::RichText::new(format!("{} · {}", active.profile_name, active.info.version))
            .weak()
            .small(),
    );
    ui.separator();

    match &active.schema.databases {
        LoadState::NotLoaded => {
            ui.add_space(4.0);
            if ui.button("Load databases").clicked() {
                actions.push(SidebarAction::RefreshDatabases);
            }
        }
        LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading databases…");
            });
        }
        LoadState::Error(e) => {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
            if ui.button("Retry").clicked() {
                actions.push(SidebarAction::RefreshDatabases);
            }
        }
        LoadState::Loaded(dbs) => {
            for db in dbs {
                render_db_node(ui, active, db, actions);
            }
        }
    }
}

fn render_db_node(
    ui: &mut egui::Ui,
    active: &ActiveConnection,
    db: &str,
    actions: &mut Vec<SidebarAction>,
) {
    let resp = egui::CollapsingHeader::new(format!("🗄 {db}"))
        .id_salt(("db", db))
        .show(ui, |ui| match active.schema.objects.get(db) {
            None | Some(LoadState::NotLoaded) => {
                actions.push(SidebarAction::RefreshDatabase(db.to_string()));
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
            }
            Some(LoadState::Loading) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
            }
            Some(LoadState::Error(e)) => {
                ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
                if ui.button("Retry").clicked() {
                    actions.push(SidebarAction::RefreshDatabase(db.to_string()));
                }
            }
            Some(LoadState::Loaded(objs)) => {
                if objs.total() == 0 {
                    ui.label(egui::RichText::new("(empty)").weak().italics());
                    return;
                }
                render_category(
                    ui,
                    db,
                    "Tables",
                    ObjectKind::Table,
                    &objs.tables,
                    actions,
                    true,
                );
                render_category(
                    ui,
                    db,
                    "Views",
                    ObjectKind::View,
                    &objs.views,
                    actions,
                    false,
                );
                render_category(
                    ui,
                    db,
                    "Procedures",
                    ObjectKind::Procedure,
                    &objs.procedures,
                    actions,
                    false,
                );
                render_category(
                    ui,
                    db,
                    "Functions",
                    ObjectKind::Function,
                    &objs.functions,
                    actions,
                    false,
                );
                render_category(
                    ui,
                    db,
                    "Triggers",
                    ObjectKind::Trigger,
                    &objs.triggers,
                    actions,
                    false,
                );
                render_category(
                    ui,
                    db,
                    "Events",
                    ObjectKind::Event,
                    &objs.events,
                    actions,
                    false,
                );
            }
        });

    resp.header_response.context_menu(|ui| {
        if ui.button("Refresh").clicked() {
            actions.push(SidebarAction::RefreshDatabase(db.to_string()));
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            actions.push(SidebarAction::CopyText(db.to_string()));
            ui.close();
        }
        ui.separator();
        if ui.button("Drop database…").clicked() {
            actions.push(SidebarAction::Confirm(ConfirmAction {
                title: format!("Drop database `{db}`"),
                message: format!(
                    "This will permanently delete the database `{db}` and ALL its objects."
                ),
                sql: format!("DROP DATABASE `{}`", db.replace('`', "``")),
                kind: PendingExec::DropDatabase { db: db.to_string() },
            }));
            ui.close();
        }
    });
}

fn render_category(
    ui: &mut egui::Ui,
    db: &str,
    label: &str,
    kind: ObjectKind,
    items: &[String],
    actions: &mut Vec<SidebarAction>,
    default_open: bool,
) {
    if items.is_empty() {
        return;
    }
    let id = egui::Id::new(("cat", db, label));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open)
        .show_header(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("{label} ({})", items.len()))
                    .strong()
                    .small(),
            );
        })
        .body(|ui| {
            for name in items {
                let resp = ui.add(
                    egui::Label::new(format!("{}  {}", kind.short_label(), name))
                        .selectable(false)
                        .sense(egui::Sense::click()),
                );
                resp.context_menu(|ui| {
                    if ui.button("Copy name").clicked() {
                        actions.push(SidebarAction::CopyText(name.clone()));
                        ui.close();
                    }
                    if ui.button("Copy CREATE statement").clicked() {
                        actions.push(SidebarAction::ShowCreate {
                            db: db.to_string(),
                            kind,
                            name: name.clone(),
                        });
                        ui.close();
                    }
                    if matches!(kind, ObjectKind::Table) {
                        ui.separator();
                        if ui.button("Truncate…").clicked() {
                            actions.push(SidebarAction::Confirm(ConfirmAction {
                                title: format!("Truncate `{name}`"),
                                message: format!("This will remove ALL rows from `{db}`.`{name}`."),
                                sql: format!(
                                    "TRUNCATE TABLE `{}`.`{}`",
                                    db.replace('`', "``"),
                                    name.replace('`', "``")
                                ),
                                kind: PendingExec::Truncate {
                                    db: db.to_string(),
                                    name: name.clone(),
                                },
                            }));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui
                        .button(format!("Drop {}…", kind.keyword().to_lowercase()))
                        .clicked()
                    {
                        actions.push(SidebarAction::Confirm(ConfirmAction {
                            title: format!("Drop {} `{}`", kind.keyword().to_lowercase(), name),
                            message: format!("This will permanently delete `{db}`.`{name}`."),
                            sql: format!(
                                "DROP {} `{}`.`{}`",
                                kind.keyword(),
                                db.replace('`', "``"),
                                name.replace('`', "``")
                            ),
                            kind: PendingExec::DropObject {
                                db: db.to_string(),
                                name: name.clone(),
                            },
                        }));
                        ui.close();
                    }
                });
            }
        });
}
