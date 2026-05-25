//! Object inspector tab with three subtabs: Structure / Data / Source.
//!
//! Each subtab is independently lazy-loaded (`LoadState`) the first time the
//! user enters it; the host app handles the actual requests by reacting to
//! the [`ObjectAction`] intents this module emits during render.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rysql_db::{ColumnInfo, ForeignKeyInfo, IndexInfo, ObjectKind};
use rysql_sql::Highlighter;

use crate::editor;
use crate::results::{self, ResultsAction, ResultsState};
use crate::state::LoadState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubTab {
    Structure,
    Data,
    Source,
}

pub struct ObjectViewState {
    pub kind: ObjectKind,
    pub db: String,
    pub name: String,
    pub sub: SubTab,
    pub columns: LoadState<Vec<ColumnInfo>>,
    pub indexes: LoadState<Vec<IndexInfo>>,
    pub foreign_keys: LoadState<Vec<ForeignKeyInfo>>,
    pub source: LoadState<String>,
    /// Backed by an entry in [`ResultsState::tabs`] keyed by the inner
    /// `tab_id`. `NotLoaded` until the user enters the Data subtab.
    pub data: LoadState<u64>,
    /// `true` while the user is editing the Source body. The Source
    /// `TextEdit` is interactive only in this mode; `source_buffer` is the
    /// working copy and `source_original` is the snapshot used by Revert
    /// and the "● modified" indicator.
    pub source_editing: bool,
    pub source_buffer: String,
    pub source_original: String,
}

impl ObjectViewState {
    pub fn new(kind: ObjectKind, db: String, name: String) -> Self {
        Self {
            kind,
            db,
            name,
            sub: default_sub(kind),
            columns: LoadState::NotLoaded,
            indexes: LoadState::NotLoaded,
            foreign_keys: LoadState::NotLoaded,
            source: LoadState::NotLoaded,
            data: LoadState::NotLoaded,
            source_editing: false,
            source_buffer: String::new(),
            source_original: String::new(),
        }
    }

    pub fn data_tab_id(&self) -> Option<u64> {
        match &self.data {
            LoadState::Loaded(id) => Some(*id),
            _ => None,
        }
    }

    pub fn source_dirty(&self) -> bool {
        self.source_editing && self.source_buffer != self.source_original
    }
}

/// Object kinds whose Source can be re-created in place via DROP + CREATE
/// (or `CREATE OR REPLACE`). Tables are excluded — they go through the
/// Structure subtab's column editor in a later iteration.
pub fn supports_source_editing(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Procedure | ObjectKind::Function | ObjectKind::View
    )
}

/// Intent emitted by [`render`]; the app issues the matching async request
/// and turns the response into a [`crate::bridge::UiEvent`] that updates the
/// corresponding `LoadState`.
#[derive(Debug, Clone)]
pub enum ObjectAction {
    LoadColumns,
    LoadIndexes,
    LoadForeignKeys,
    LoadSource,
    LoadData,
    /// User clicked Save on the Source editor. Carries the body to send to
    /// the server (the app prepends the matching `DROP ... IF EXISTS`).
    SaveSource(String),
    /// User clicked `+ Add column` below the Columns table. The app opens
    /// the column-edit modal seeded for Add.
    AddColumn,
    /// User clicked `[Drop]` on a column row. The app builds the DROP SQL
    /// and pushes the destructive-confirm modal.
    DropColumn {
        name: String,
    },
}

pub fn supports_structure(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Table | ObjectKind::View)
}

pub fn supports_data(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Table | ObjectKind::View)
}

pub fn supports_foreign_keys(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Table)
}

/// Whether the Structure subtab should expose Add / Drop / Modify column
/// affordances. Views are listed by `information_schema.COLUMNS` but they
/// can't be `ALTER`-ed column-by-column; the user must recreate the view
/// from the Source subtab instead.
pub fn supports_column_editing(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Table)
}

fn default_sub(kind: ObjectKind) -> SubTab {
    if supports_structure(kind) {
        SubTab::Structure
    } else {
        SubTab::Source
    }
}

fn kind_icon(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "🗃",
        ObjectKind::View => "👁",
        ObjectKind::Procedure => "⚙",
        ObjectKind::Function => "ƒ",
        ObjectKind::Trigger => "⚡",
        ObjectKind::Event => "⏰",
    }
}

fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "Table",
        ObjectKind::View => "View",
        ObjectKind::Procedure => "Procedure",
        ObjectKind::Function => "Function",
        ObjectKind::Trigger => "Trigger",
        ObjectKind::Event => "Event",
    }
}

pub fn render(
    ui: &mut egui::Ui,
    state: &mut ObjectViewState,
    results: &mut ResultsState,
    highlighter: &mut Highlighter,
    results_actions: &mut Vec<ResultsAction>,
) -> Vec<ObjectAction> {
    let mut actions: Vec<ObjectAction> = Vec::new();

    egui::Panel::top(egui::Id::new(("obj-toolbar", &state.db, &state.name)))
        .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(8, 4)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} {} · {}.{}",
                        kind_icon(state.kind),
                        kind_label(state.kind),
                        state.db,
                        state.name,
                    ))
                    .strong(),
                );
                ui.separator();
                if supports_structure(state.kind)
                    && ui
                        .selectable_label(state.sub == SubTab::Structure, "Structure")
                        .clicked()
                {
                    state.sub = SubTab::Structure;
                }
                if supports_data(state.kind)
                    && ui
                        .selectable_label(state.sub == SubTab::Data, "Data")
                        .clicked()
                {
                    state.sub = SubTab::Data;
                }
                if ui
                    .selectable_label(state.sub == SubTab::Source, "Source")
                    .clicked()
                {
                    state.sub = SubTab::Source;
                }
            });
        });

    match state.sub {
        SubTab::Structure => render_structure(ui, state, &mut actions),
        SubTab::Data => render_data(ui, state, results, &mut actions, results_actions),
        SubTab::Source => render_source(ui, state, highlighter, &mut actions),
    }

    actions
}

fn render_structure(
    ui: &mut egui::Ui,
    state: &mut ObjectViewState,
    actions: &mut Vec<ObjectAction>,
) {
    // Kick off lazy loads on first entry.
    if matches!(state.columns, LoadState::NotLoaded) {
        state.columns = LoadState::Loading;
        actions.push(ObjectAction::LoadColumns);
    }
    if matches!(state.indexes, LoadState::NotLoaded) {
        state.indexes = LoadState::Loading;
        actions.push(ObjectAction::LoadIndexes);
    }
    if supports_foreign_keys(state.kind) && matches!(state.foreign_keys, LoadState::NotLoaded) {
        state.foreign_keys = LoadState::Loading;
        actions.push(ObjectAction::LoadForeignKeys);
    }

    let editable = supports_column_editing(state.kind);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Columns").strong());
            render_columns(ui, &state.columns, editable, actions);

            if editable {
                ui.add_space(4.0);
                if ui.button("+ Add column").clicked() {
                    actions.push(ObjectAction::AddColumn);
                }
            }

            ui.add_space(12.0);
            ui.label(egui::RichText::new("Indexes").strong());
            render_indexes(ui, &state.indexes);

            if supports_foreign_keys(state.kind) {
                ui.add_space(12.0);
                egui::CollapsingHeader::new("Foreign keys")
                    .id_salt(("obj-fks", &state.db, &state.name))
                    .default_open(false)
                    .show(ui, |ui| {
                        render_fks(ui, &state.foreign_keys);
                    });
            }
        });
}

fn render_columns(
    ui: &mut egui::Ui,
    columns: &LoadState<Vec<ColumnInfo>>,
    editable: bool,
    actions: &mut Vec<ObjectAction>,
) {
    match columns {
        LoadState::NotLoaded | LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading columns…");
            });
        }
        LoadState::Error(e) => {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
        }
        LoadState::Loaded(cols) if cols.is_empty() => {
            ui.label(egui::RichText::new("(no columns)").weak().italics());
        }
        LoadState::Loaded(cols) => {
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let mut drop_request: Option<String> = None;
            let mut builder = TableBuilder::new(ui)
                .id_salt(("obj-cols",))
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(180.0).at_least(80.0).resizable(true)) // name
                .column(Column::initial(180.0).at_least(60.0).resizable(true)) // type
                .column(Column::initial(60.0).at_least(40.0)) // null
                .column(Column::initial(40.0).at_least(30.0)) // pk
                .column(Column::initial(120.0).at_least(40.0).resizable(true)) // default
                .column(Column::initial(180.0).at_least(40.0).resizable(true)) // extra
                .column(Column::initial(160.0).at_least(60.0).resizable(true)); // comment
            if editable {
                builder = builder.column(Column::exact(70.0)); // actions
            } else {
                builder = builder.column(Column::remainder().at_least(80.0));
            }
            let headers: &[&str] = if editable {
                &[
                    "Name", "Type", "Null", "PK", "Default", "Extra", "Comment", "",
                ]
            } else {
                &["Name", "Type", "Null", "PK", "Default", "Extra", "Comment"]
            };
            builder
                .header(row_h, |mut header| {
                    for label in headers {
                        header.col(|ui| {
                            ui.label(egui::RichText::new(*label).strong().small());
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, cols.len(), |mut row| {
                        let c = &cols[row.index()];
                        row.col(|ui| {
                            ui.add(egui::Label::new(&c.name).truncate());
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(&c.data_type).monospace())
                                    .truncate(),
                            );
                        });
                        row.col(|ui| {
                            ui.label(if c.nullable { "YES" } else { "NO" });
                        });
                        row.col(|ui| {
                            ui.label(if c.is_pk { "✓" } else { "" });
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(c.default_value.clone().unwrap_or_default())
                                    .truncate(),
                            );
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(&c.extra).truncate());
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(&c.comment).truncate());
                        });
                        if editable {
                            row.col(|ui| {
                                let btn = egui::Button::new(
                                    egui::RichText::new("Drop")
                                        .small()
                                        .color(egui::Color32::from_rgb(0xe5, 0x73, 0x73)),
                                );
                                if ui.add(btn).on_hover_text("Drop this column").clicked() {
                                    drop_request = Some(c.name.clone());
                                }
                            });
                        } else {
                            row.col(|_ui| {});
                        }
                    });
                });
            if let Some(name) = drop_request {
                actions.push(ObjectAction::DropColumn { name });
            }
        }
    }
}

fn render_indexes(ui: &mut egui::Ui, indexes: &LoadState<Vec<IndexInfo>>) {
    match indexes {
        LoadState::NotLoaded | LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading indexes…");
            });
        }
        LoadState::Error(e) => {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
        }
        LoadState::Loaded(idx) if idx.is_empty() => {
            ui.label(egui::RichText::new("(no indexes)").weak().italics());
        }
        LoadState::Loaded(idx) => {
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            TableBuilder::new(ui)
                .id_salt(("obj-idx",))
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(180.0).at_least(80.0).resizable(true))
                .column(Column::initial(260.0).at_least(80.0).resizable(true))
                .column(Column::initial(80.0).at_least(40.0))
                .column(Column::remainder().at_least(60.0))
                .header(row_h, |mut header| {
                    for label in ["Name", "Columns", "Unique", "Type"] {
                        header.col(|ui| {
                            ui.label(egui::RichText::new(label).strong().small());
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, idx.len(), |mut row| {
                        let i = &idx[row.index()];
                        row.col(|ui| {
                            ui.add(egui::Label::new(&i.name).truncate());
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(i.columns.join(", ")).monospace(),
                                )
                                .truncate(),
                            );
                        });
                        row.col(|ui| {
                            ui.label(if i.unique { "✓" } else { "" });
                        });
                        row.col(|ui| {
                            ui.label(&i.index_type);
                        });
                    });
                });
        }
    }
}

fn render_fks(ui: &mut egui::Ui, fks: &LoadState<Vec<ForeignKeyInfo>>) {
    match fks {
        LoadState::NotLoaded | LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading foreign keys…");
            });
        }
        LoadState::Error(e) => {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
        }
        LoadState::Loaded(list) if list.is_empty() => {
            ui.label(egui::RichText::new("(none)").weak().italics());
        }
        LoadState::Loaded(list) => {
            for fk in list {
                ui.label(egui::RichText::new(&fk.name).strong());
                ui.label(format!(
                    "({}) → `{}`.`{}` ({})",
                    fk.columns.join(", "),
                    fk.ref_db,
                    fk.ref_table,
                    fk.ref_columns.join(", "),
                ));
                ui.label(
                    egui::RichText::new(format!(
                        "ON DELETE {} · ON UPDATE {}",
                        fk.on_delete, fk.on_update
                    ))
                    .weak()
                    .small(),
                );
                ui.add_space(4.0);
            }
        }
    }
}

fn render_data(
    ui: &mut egui::Ui,
    state: &mut ObjectViewState,
    results: &mut ResultsState,
    actions: &mut Vec<ObjectAction>,
    results_actions: &mut Vec<ResultsAction>,
) {
    match &state.data {
        LoadState::NotLoaded => {
            state.data = LoadState::Loading;
            actions.push(ObjectAction::LoadData);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading data…");
            });
        }
        LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading data…");
            });
        }
        LoadState::Error(e) => {
            let msg = e.clone();
            ui.vertical(|ui| {
                ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), &msg);
                if ui.button("Retry").clicked() {
                    state.data = LoadState::Loading;
                    actions.push(ObjectAction::LoadData);
                }
            });
        }
        LoadState::Loaded(tab_id) => {
            let tab_id = *tab_id;
            if results.find_by_id(tab_id).is_some() {
                results::render_one(ui, results, tab_id, results_actions);
            } else {
                // The backing ResultTab was evicted; clear state and re-load.
                state.data = LoadState::NotLoaded;
                ui.label(egui::RichText::new("Data was evicted; reloading…").weak());
            }
        }
    }
}

fn render_source(
    ui: &mut egui::Ui,
    state: &mut ObjectViewState,
    highlighter: &mut Highlighter,
    actions: &mut Vec<ObjectAction>,
) {
    if matches!(state.source, LoadState::NotLoaded) {
        state.source = LoadState::Loading;
        actions.push(ObjectAction::LoadSource);
    }

    let editing = state.source_editing;
    let editable_kind = supports_source_editing(state.kind);
    let mut save_clicked = false;

    // Edit / Save / Revert toolbar — only when the kind supports it and the
    // DDL has actually loaded (no point in editing a spinner).
    if editable_kind && matches!(&state.source, LoadState::Loaded(_)) {
        egui::Panel::top(egui::Id::new((
            "obj-source-toolbar",
            &state.db,
            &state.name,
        )))
        .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(8, 4)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if !editing {
                    if ui.button("Edit").clicked() {
                        if let LoadState::Loaded(ddl) = &state.source {
                            state.source_buffer = ddl.clone();
                            state.source_original = ddl.clone();
                            state.source_editing = true;
                        }
                    }
                } else {
                    if ui.button("Save").clicked() {
                        save_clicked = true;
                    }
                    if ui.button("Revert").clicked() {
                        state.source_editing = false;
                        state.source_buffer.clear();
                        state.source_original.clear();
                    }
                    if state.source_dirty() {
                        ui.label(
                            egui::RichText::new("● modified")
                                .color(egui::Color32::from_rgb(0xff, 0xb7, 0x4d))
                                .small(),
                        );
                    }
                }
            });
        });
    }

    if save_clicked {
        actions.push(ObjectAction::SaveSource(state.source_buffer.clone()));
    }

    match &mut state.source {
        LoadState::NotLoaded | LoadState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading source…");
            });
        }
        LoadState::Error(e) => {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), &*e);
        }
        LoadState::Loaded(ddl) => {
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                let mut job = editor::build_layout_job(text.as_str(), highlighter, ui);
                job.wrap.max_width = wrap_width;
                ui.ctx().fonts_mut(|f| f.layout_job(job))
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if editing {
                        ui.add(
                            egui::TextEdit::multiline(&mut state.source_buffer)
                                .font(egui::TextStyle::Monospace)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(20)
                                .layouter(&mut layouter),
                        );
                    } else {
                        let mut buf = ddl.clone();
                        ui.add(
                            egui::TextEdit::multiline(&mut buf)
                                .font(egui::TextStyle::Monospace)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(20)
                                .interactive(false)
                                .layouter(&mut layouter),
                        );
                    }
                });
        }
    }
}
