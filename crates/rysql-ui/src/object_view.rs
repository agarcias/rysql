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

/// Comparison operator offered in the Data filter builder. Mapped to SQL by
/// [`build_where_clause`]; `IsNull`/`IsNotNull` take no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    StartsWith,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    const ALL: [FilterOp; 10] = [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Lt,
        FilterOp::Le,
        FilterOp::Gt,
        FilterOp::Ge,
        FilterOp::Contains,
        FilterOp::StartsWith,
        FilterOp::IsNull,
        FilterOp::IsNotNull,
    ];

    fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "≠",
            FilterOp::Lt => "<",
            FilterOp::Le => "≤",
            FilterOp::Gt => ">",
            FilterOp::Ge => "≥",
            FilterOp::Contains => "contains",
            FilterOp::StartsWith => "starts with",
            FilterOp::IsNull => "is null",
            FilterOp::IsNotNull => "is not null",
        }
    }

    /// `false` for the unary `IS [NOT] NULL` operators, which ignore the
    /// value input.
    fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }
}

/// One AND-combined condition in the Data filter builder.
#[derive(Debug, Clone)]
pub struct FilterCond {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
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
    /// Editable conditions in the Data filter builder. Persist across data
    /// reloads (they live here, not on the result tab).
    pub data_filters: Vec<FilterCond>,
    /// The WHERE body currently in effect (without the `WHERE` keyword), or
    /// `None` when unfiltered. Threaded into every `load_object_data` so the
    /// filter survives pagination, refreshes and tab eviction.
    pub applied_where: Option<String>,
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
            data_filters: Vec::new(),
            applied_where: None,
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

/// Object kinds whose Source can be re-created in place via DROP + CREATE.
/// Tables go through the Structure subtab's per-column editor instead;
/// triggers and events are not supported yet (they require re-binding to
/// their host table or event scheduler).
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
    /// User clicked `[Edit]` on a column row. The app looks up the column
    /// in `state.columns` and opens the modal in Modify mode prefilled
    /// with the current values.
    ModifyColumn {
        name: String,
    },
    /// User clicked Apply in the Data filter builder. The app rebuilds the
    /// WHERE from `state.data_filters` and reloads the Data subtab.
    ApplyDataFilter,
    /// User clicked Clear; drop all conditions and reload unfiltered.
    ClearDataFilter,
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
                        state.kind.short_label(),
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
            let mut edit_request: Option<String> = None;
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
                builder = builder.column(Column::exact(120.0)); // actions
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
                                ui.horizontal(|ui| {
                                    let edit_btn =
                                        egui::Button::new(egui::RichText::new("Edit").small());
                                    if ui
                                        .add(edit_btn)
                                        .on_hover_text("Modify this column")
                                        .clicked()
                                    {
                                        edit_request = Some(c.name.clone());
                                    }
                                    let drop_btn = egui::Button::new(
                                        egui::RichText::new("Drop")
                                            .small()
                                            .color(egui::Color32::from_rgb(0xe5, 0x73, 0x73)),
                                    );
                                    if ui.add(drop_btn).on_hover_text("Drop this column").clicked()
                                    {
                                        drop_request = Some(c.name.clone());
                                    }
                                });
                            });
                        } else {
                            row.col(|_ui| {});
                        }
                    });
                });
            if let Some(name) = edit_request {
                actions.push(ObjectAction::ModifyColumn { name });
            }
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
    // The filter builder needs column metadata for its column dropdown and
    // type-aware quoting. Lazy-load it on first entry (shared with the
    // Structure subtab's cache, so this is a no-op if already loaded).
    if matches!(state.columns, LoadState::NotLoaded) {
        state.columns = LoadState::Loading;
        actions.push(ObjectAction::LoadColumns);
    }
    render_data_filter_bar(ui, state, actions);

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

/// Column-filter builder pinned above the Data grid. Conditions are AND'd
/// and applied server-side (see [`build_where_clause`]); the actual reload
/// is driven by the [`ObjectAction::ApplyDataFilter`] / `ClearDataFilter`
/// intents this emits.
fn render_data_filter_bar(
    ui: &mut egui::Ui,
    state: &mut ObjectViewState,
    actions: &mut Vec<ObjectAction>,
) {
    let panel_id = egui::Id::new(("data-filter-bar", state.db.clone(), state.name.clone()));
    // Owned snapshot of the column list so the mutable closure below doesn't
    // also borrow `state.columns`.
    let columns: Vec<ColumnInfo> = match &state.columns {
        LoadState::Loaded(c) => c.clone(),
        _ => Vec::new(),
    };
    let columns_ready = !columns.is_empty();
    let is_filtered = state.applied_where.is_some();

    egui::Panel::top(panel_id)
        .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(8, 4)))
        .show_inside(ui, |ui| {
            let mut apply = false;
            let mut clear = false;
            let mut remove: Option<usize> = None;

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Filters").strong());
                let add = egui::Button::new("+ Add condition");
                if ui
                    .add_enabled(columns_ready, add)
                    .on_hover_text(if columns_ready {
                        "Add a column condition"
                    } else {
                        "Loading columns…"
                    })
                    .clicked()
                {
                    let first = columns.first().map(|c| c.name.clone()).unwrap_or_default();
                    state.data_filters.push(FilterCond {
                        column: first,
                        op: FilterOp::Eq,
                        value: String::new(),
                    });
                }
                if !state.data_filters.is_empty() {
                    ui.separator();
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Clear").clicked() {
                        clear = true;
                    }
                }
                if is_filtered {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("● filtered")
                            .color(egui::Color32::from_rgb(0x81, 0xc7, 0x84))
                            .small(),
                    );
                }
            });

            for (i, cond) in state.data_filters.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let col_label = if cond.column.is_empty() {
                        "(column)".to_string()
                    } else {
                        cond.column.clone()
                    };
                    egui::ComboBox::from_id_salt(("filter-col", i))
                        .selected_text(col_label)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for c in &columns {
                                ui.selectable_value(&mut cond.column, c.name.clone(), &c.name);
                            }
                        });
                    egui::ComboBox::from_id_salt(("filter-op", i))
                        .selected_text(cond.op.label())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for op in FilterOp::ALL {
                                ui.selectable_value(&mut cond.op, op, op.label());
                            }
                        });
                    if cond.op.needs_value() {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut cond.value)
                                .desired_width(220.0)
                                .hint_text("value")
                                .font(egui::TextStyle::Monospace),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            apply = true;
                        }
                    } else {
                        let mut placeholder = String::new();
                        ui.add_enabled(
                            false,
                            egui::TextEdit::singleline(&mut placeholder).desired_width(220.0),
                        );
                    }
                    if ui
                        .button("✕")
                        .on_hover_text("Remove this condition")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            }

            if let Some(i) = remove {
                state.data_filters.remove(i);
            }
            if apply {
                actions.push(ObjectAction::ApplyDataFilter);
            }
            if clear {
                actions.push(ObjectAction::ClearDataFilter);
            }
        });
}

/// Build the WHERE body (no `WHERE` keyword) for the active conditions,
/// AND-combined. Returns `None` when nothing is active (no column picked, or
/// an empty value on a value-requiring operator) so the caller loads
/// unfiltered. Values are escaped; numeric columns with numeric input are
/// emitted unquoted, everything else single-quoted.
pub fn build_where_clause(filters: &[FilterCond], columns: &[ColumnInfo]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for f in filters {
        if f.column.is_empty() {
            continue;
        }
        let value = f.value.trim();
        if f.op.needs_value() && value.is_empty() {
            continue;
        }
        let col = format!("`{}`", f.column.replace('`', "``"));
        let numeric = columns
            .iter()
            .find(|c| c.name == f.column)
            .is_some_and(|c| is_numeric_type(&c.data_type));
        let part = match f.op {
            FilterOp::Eq => format!("{col} = {}", sql_scalar(value, numeric)),
            FilterOp::Ne => format!("{col} <> {}", sql_scalar(value, numeric)),
            FilterOp::Lt => format!("{col} < {}", sql_scalar(value, numeric)),
            FilterOp::Le => format!("{col} <= {}", sql_scalar(value, numeric)),
            FilterOp::Gt => format!("{col} > {}", sql_scalar(value, numeric)),
            FilterOp::Ge => format!("{col} >= {}", sql_scalar(value, numeric)),
            FilterOp::Contains => format!("{col} LIKE {}", like_literal(value, true, true)),
            FilterOp::StartsWith => format!("{col} LIKE {}", like_literal(value, false, true)),
            FilterOp::IsNull => format!("{col} IS NULL"),
            FilterOp::IsNotNull => format!("{col} IS NOT NULL"),
        };
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

fn is_numeric_type(data_type: &str) -> bool {
    let lower = data_type.trim().to_ascii_lowercase();
    [
        "tinyint", "smallint", "mediumint", "bigint", "int", "integer", "decimal", "numeric",
        "float", "double", "real", "bit", "year",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
}

/// Quote a scalar for a comparison operator: numeric columns fed numeric
/// input pass through bare; otherwise single-quote and escape.
fn sql_scalar(value: &str, numeric: bool) -> String {
    if numeric && value.parse::<f64>().is_ok() {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
    }
}

/// Build a single-quoted LIKE pattern with the user's text matched
/// literally (`%`/`_` in the input are escaped) and `%` wildcards added on
/// the requested sides.
fn like_literal(value: &str, lead: bool, trail: bool) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let mut out = String::from("'");
    if lead {
        out.push('%');
    }
    out.push_str(&escaped);
    if trail {
        out.push('%');
    }
    out.push('\'');
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, data_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            extra: String::new(),
            comment: String::new(),
        }
    }

    fn cond(column: &str, op: FilterOp, value: &str) -> FilterCond {
        FilterCond {
            column: column.to_string(),
            op,
            value: value.to_string(),
        }
    }

    #[test]
    fn empty_filters_yield_no_clause() {
        assert_eq!(build_where_clause(&[], &[]), None);
    }

    #[test]
    fn blank_value_is_skipped() {
        let cols = [col("name", "varchar(255)")];
        let f = [cond("name", FilterOp::Eq, "   ")];
        assert_eq!(build_where_clause(&f, &cols), None);
    }

    #[test]
    fn numeric_column_emits_bare_literal() {
        let cols = [col("age", "int")];
        let f = [cond("age", FilterOp::Gt, "30")];
        assert_eq!(build_where_clause(&f, &cols).as_deref(), Some("`age` > 30"));
    }

    #[test]
    fn numeric_column_non_numeric_input_is_quoted() {
        let cols = [col("age", "int")];
        let f = [cond("age", FilterOp::Eq, "n/a")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`age` = 'n/a'")
        );
    }

    #[test]
    fn text_column_is_quoted_and_escaped() {
        let cols = [col("name", "varchar(255)")];
        let f = [cond("name", FilterOp::Eq, "O'Brien")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`name` = 'O''Brien'")
        );
    }

    #[test]
    fn contains_wraps_and_escapes_wildcards() {
        let cols = [col("name", "varchar(255)")];
        let f = [cond("name", FilterOp::Contains, "50%_off")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some(r"`name` LIKE '%50\%\_off%'")
        );
    }

    #[test]
    fn starts_with_only_trails() {
        let cols = [col("name", "varchar(255)")];
        let f = [cond("name", FilterOp::StartsWith, "Ana")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`name` LIKE 'Ana%'")
        );
    }

    #[test]
    fn null_ops_ignore_value_and_need_no_input() {
        let cols = [col("deleted_at", "datetime")];
        let f = [cond("deleted_at", FilterOp::IsNull, "ignored")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`deleted_at` IS NULL")
        );
    }

    #[test]
    fn multiple_conditions_are_anded() {
        let cols = [col("status", "varchar(20)"), col("age", "int")];
        let f = [
            cond("status", FilterOp::Eq, "active"),
            cond("age", FilterOp::Ge, "18"),
        ];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`status` = 'active' AND `age` >= 18")
        );
    }

    #[test]
    fn backtick_in_column_is_escaped() {
        let cols = [col("we`ird", "int")];
        let f = [cond("we`ird", FilterOp::Eq, "1")];
        assert_eq!(
            build_where_clause(&f, &cols).as_deref(),
            Some("`we``ird` = 1")
        );
    }

    #[test]
    fn unselected_column_is_skipped() {
        let cols = [col("age", "int")];
        let f = [cond("", FilterOp::Eq, "5")];
        assert_eq!(build_where_clause(&f, &cols), None);
    }
}
