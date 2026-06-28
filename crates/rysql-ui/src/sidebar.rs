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
    /// Double-click on a schema-tree item: open (or focus) its dock tab.
    OpenObject {
        db: String,
        kind: ObjectKind,
        name: String,
    },
    /// Open the export dialog for the named database.
    ExportDatabase(String),
    Confirm(ConfirmAction),
}

pub struct SidebarInput<'a> {
    pub profiles: &'a [ConnectionProfile],
    pub active: Option<&'a ActiveConnection>,
    pub in_flight: &'a std::collections::HashSet<String>,
    /// Substring filter mutated in place by the schema-tree search input.
    /// Empty means "show everything".
    pub filter: &'a mut String,
}

pub fn render(ui: &mut egui::Ui, input: SidebarInput<'_>) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    let SidebarInput {
        profiles,
        active,
        in_flight,
        filter,
    } = input;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Connections");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+").on_hover_text("New connection").clicked() {
                    actions.push(SidebarAction::NewConnection);
                }
            });
        });
        ui.separator();

        if profiles.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.label("No connections yet.");
                if ui.button("New connection…").clicked() {
                    actions.push(SidebarAction::NewConnection);
                }
            });
            return;
        }

        // One node per connection. The active one expands into its schema
        // tree (databases → categories → objects); inactive ones are plain
        // rows — they have no loaded objects to nest until connected.
        let active_name = active.map(|a| a.profile_name.as_str());
        for profile in profiles {
            let is_active = active_name == Some(profile.name.as_str());
            let is_busy = in_flight.contains(&profile.name);
            if is_active {
                if let Some(active) = active {
                    connection_node(ui, profile, active, is_busy, filter, &mut actions);
                }
            } else {
                inactive_connection_row(ui, profile, is_busy, &mut actions);
            }
            ui.add_space(2.0);
        }
    });
    actions
}

/// `user@host:port[/db]` endpoint line shown under a connection.
fn endpoint_label(profile: &ConnectionProfile) -> String {
    format!(
        "{}@{}:{}{}",
        profile.user,
        profile.host,
        profile.port,
        profile
            .database
            .as_deref()
            .map(|d| format!("/{d}"))
            .unwrap_or_default()
    )
}

/// A connected profile: a collapsing node (open by default) whose body holds
/// the endpoint/version subtitle and the full schema tree.
fn connection_node(
    ui: &mut egui::Ui,
    profile: &ConnectionProfile,
    active: &ActiveConnection,
    is_busy: bool,
    filter: &mut String,
    actions: &mut Vec<SidebarAction>,
) {
    let resp =
        egui::CollapsingHeader::new(egui::RichText::new(format!("● {}", profile.name)).strong())
            .id_salt(("conn", &profile.name))
            .default_open(true)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(endpoint_label(profile)).weak().small());
                schema_body(ui, active, filter, actions);
            });

    if is_busy {
        ui.spinner();
    }

    resp.header_response.context_menu(|ui| {
        if ui.button("Disconnect").clicked() {
            actions.push(SidebarAction::Disconnect);
            ui.close();
        }
        if ui.button("Refresh databases").clicked() {
            actions.push(SidebarAction::RefreshDatabases);
            ui.close();
        }
        ui.separator();
        if ui.button("Delete").clicked() {
            actions.push(SidebarAction::DeleteProfile(profile.name.clone()));
            ui.close();
        }
    });
}

/// A profile that isn't the active connection: a simple row. Double-click or
/// the context menu connects; there's nothing to expand yet.
fn inactive_connection_row(
    ui: &mut egui::Ui,
    profile: &ConnectionProfile,
    is_busy: bool,
    actions: &mut Vec<SidebarAction>,
) {
    ui.horizontal(|ui| {
        let resp = ui.selectable_label(false, format!("○ {}", profile.name));
        if resp.double_clicked() && !is_busy {
            actions.push(SidebarAction::Connect(profile.name.clone()));
        }
        resp.context_menu(|ui| {
            if ui
                .add_enabled(!is_busy, egui::Button::new("Connect"))
                .clicked()
            {
                actions.push(SidebarAction::Connect(profile.name.clone()));
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
    ui.label(egui::RichText::new(endpoint_label(profile)).weak().small());
}

/// Schema tree body, rendered inside the active connection's node: a
/// version/refresh row, the name filter, and the databases tree.
fn schema_body(
    ui: &mut egui::Ui,
    active: &ActiveConnection,
    filter: &mut String,
    actions: &mut Vec<SidebarAction>,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Schema · {}", active.info.version))
                .weak()
                .small(),
        );
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

    // Filter input — substring match against DB names and object names.
    // Empty → existing render path; non-empty → flat filtered view.
    //
    // We compute the input's `desired_width` from the row's currently
    // available width instead of asking for `INFINITY`. Otherwise the
    // request propagates up to the `SidePanel` and grows the whole
    // sidebar to fit the (effectively unbounded) request.
    ui.horizontal(|ui| {
        let clear_reserved = if filter.is_empty() { 0.0 } else { 28.0 };
        let avail = (ui.available_width() - clear_reserved).max(40.0);
        ui.add(
            egui::TextEdit::singleline(filter)
                .hint_text("Filter…")
                .desired_width(avail),
        );
        if !filter.is_empty() && ui.small_button("×").on_hover_text("Clear filter").clicked() {
            filter.clear();
        }
    });
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
            let needle = filter.trim().to_ascii_lowercase();
            if needle.is_empty() {
                for db in dbs {
                    render_db_node(ui, active, db, actions);
                }
            } else {
                let mut any_match = false;
                for db in dbs {
                    if render_db_filtered(ui, active, db, &needle, actions) {
                        any_match = true;
                    }
                }
                if !any_match {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("(no matches)").weak().italics().small());
                }
            }
        }
    }
}

/// Render a database node when the filter is active. Returns `true` if
/// anything was rendered (DB name match OR ≥ 1 object name match), so
/// the caller can show a "(no matches)" hint when nothing matched.
fn render_db_filtered(
    ui: &mut egui::Ui,
    active: &ActiveConnection,
    db: &str,
    needle: &str,
    actions: &mut Vec<SidebarAction>,
) -> bool {
    let db_match = db.to_ascii_lowercase().contains(needle);

    // If the DB name itself matches, surface the full tree.
    if db_match {
        render_db_node(ui, active, db, actions);
        return true;
    }

    let Some(LoadState::Loaded(objs)) = active.schema.objects.get(db) else {
        // The DB's objects aren't loaded yet — we can't decide if any
        // item matches. Skip silently; the user can expand the unfiltered
        // tree to trigger the load.
        return false;
    };

    let categories: &[(&str, ObjectKind, &[String])] = &[
        ("Tables", ObjectKind::Table, objs.tables.as_slice()),
        ("Views", ObjectKind::View, objs.views.as_slice()),
        (
            "Procedures",
            ObjectKind::Procedure,
            objs.procedures.as_slice(),
        ),
        ("Functions", ObjectKind::Function, objs.functions.as_slice()),
        ("Triggers", ObjectKind::Trigger, objs.triggers.as_slice()),
        ("Events", ObjectKind::Event, objs.events.as_slice()),
    ];

    let any_hit = categories.iter().any(|(_, _, items)| {
        items
            .iter()
            .any(|n| n.to_ascii_lowercase().contains(needle))
    });
    if !any_hit {
        return false;
    }

    ui.add_space(2.0);
    ui.label(egui::RichText::new(format!("🗄 {db}")).strong());
    for (label, kind, items) in categories {
        let hits: Vec<&String> = items
            .iter()
            .filter(|n| n.to_ascii_lowercase().contains(needle))
            .collect();
        if hits.is_empty() {
            continue;
        }
        ui.label(
            egui::RichText::new(format!("  {label} ({})", hits.len()))
                .strong()
                .small(),
        );
        for name in hits {
            render_object_item(ui, db, *kind, name, actions);
        }
    }
    true
}

/// Single schema-tree item rendering, shared between the collapsing
/// `render_category` and the flat filtered view.
fn render_object_item(
    ui: &mut egui::Ui,
    db: &str,
    kind: ObjectKind,
    name: &str,
    actions: &mut Vec<SidebarAction>,
) {
    let resp = ui.add(
        egui::Label::new(format!("   {}  {}", kind.short_label(), name))
            .selectable(false)
            .truncate()
            .show_tooltip_when_elided(true)
            .sense(egui::Sense::click()),
    );
    if resp.double_clicked() {
        actions.push(SidebarAction::OpenObject {
            db: db.to_string(),
            kind,
            name: name.to_string(),
        });
    }
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            actions.push(SidebarAction::OpenObject {
                db: db.to_string(),
                kind,
                name: name.to_string(),
            });
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            actions.push(SidebarAction::CopyText(name.to_string()));
            ui.close();
        }
        if ui.button("Copy CREATE statement").clicked() {
            actions.push(SidebarAction::ShowCreate {
                db: db.to_string(),
                kind,
                name: name.to_string(),
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
                        name: name.to_string(),
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
                    name: name.to_string(),
                },
            }));
            ui.close();
        }
    });
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
        if ui.button("Export database…").clicked() {
            actions.push(SidebarAction::ExportDatabase(db.to_string()));
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
                render_object_item(ui, db, kind, name, actions);
            }
        });
}
