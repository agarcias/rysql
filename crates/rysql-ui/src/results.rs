//! Results pane: tabs of result sets rendered with `egui_extras::TableBuilder`.
//!
//! Phase 4b features: pagination (auto-LIMIT + fetch next), BLOB viewer,
//! export to clipboard (CSV / TSV / INSERT), and edit-in-place by primary key.

use std::collections::HashSet;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rysql_db::{Cell, ColumnInfo, ColumnMeta, QueryResult};

use crate::state::LoadState;

pub const DEFAULT_PAGE_SIZE: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Tsv,
    Insert,
}

#[derive(Debug, Clone)]
pub struct EditableTarget {
    pub db: String,
    pub table: String,
    pub pk_cols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResultTab {
    pub tab_id: u64,
    pub label: String,
    pub result: QueryResult,
    /// Index permutation: `order[i]` is the original row index displayed at row `i`.
    pub order: Vec<usize>,
    pub sort_by: Option<(usize, SortOrder)>,
    /// Original SQL the user typed (no auto-LIMIT applied).
    pub sql: String,
    /// Page size used for auto-pagination.
    pub page_size: u64,
    /// Where the next page starts.
    pub next_offset: u64,
    /// True when a previous fetch returned a full page (more rows may exist).
    pub has_more: bool,
    /// Set after origin + PK detection succeeds.
    pub editable: Option<EditableTarget>,
    /// Indices into `result.rows` (NOT display order) of rows the user
    /// ticked via the leading checkbox column. Stored as original indices
    /// so a sort change doesn't disturb the selection.
    pub selection: HashSet<usize>,
}

impl ResultTab {
    pub fn new(tab_id: u64, label: String, sql: String, result: QueryResult) -> Self {
        let order = (0..result.rows.len()).collect();
        Self {
            tab_id,
            label,
            order,
            page_size: DEFAULT_PAGE_SIZE,
            next_offset: result.rows.len() as u64,
            has_more: false,
            editable: None,
            selection: HashSet::new(),
            result,
            sort_by: None,
            sql,
        }
    }

    pub fn apply_sort(&mut self) {
        match self.sort_by {
            None => {
                self.order = (0..self.result.rows.len()).collect();
            }
            Some((col, dir)) => {
                let rows = &self.result.rows;
                self.order = (0..rows.len()).collect();
                self.order.sort_by(|&a, &b| {
                    let ca = rows.get(a).and_then(|r| r.get(col));
                    let cb = rows.get(b).and_then(|r| r.get(col));
                    cmp_cells(ca, cb)
                });
                if matches!(dir, SortOrder::Desc) {
                    self.order.reverse();
                }
            }
        }
    }

    fn cycle_sort(&mut self, col: usize) {
        self.sort_by = match self.sort_by {
            Some((c, SortOrder::Asc)) if c == col => Some((col, SortOrder::Desc)),
            Some((c, SortOrder::Desc)) if c == col => None,
            _ => Some((col, SortOrder::Asc)),
        };
        self.apply_sort();
    }

    /// Append rows from a follow-up "fetch next page" query, preserving sort.
    pub fn append(&mut self, more: QueryResult, page_size: u64) {
        let added = more.rows.len() as u64;
        self.result.rows.extend(more.rows);
        self.result.elapsed += more.elapsed;
        self.next_offset += added;
        self.has_more = added >= page_size;
        self.apply_sort();
    }

    /// Determine if the result's columns all originate from the same `(db, table)`.
    /// Returns the candidate target (without pk_cols filled).
    pub fn detect_single_table(&self) -> Option<EditableTarget> {
        let mut origin: Option<(String, String)> = None;
        for col in &self.result.columns {
            match &col.origin {
                None => return None,
                Some((db, tbl)) => match &origin {
                    None => origin = Some((db.clone(), tbl.clone())),
                    Some(prev) if prev.0 == *db && prev.1 == *tbl => {}
                    Some(_) => return None,
                },
            }
        }
        let (db, table) = origin?;
        Some(EditableTarget {
            db,
            table,
            pk_cols: Vec::new(),
        })
    }
}

fn cmp_cells(a: Option<&Cell>, b: Option<&Cell>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) | (Some(Cell::Null), Some(Cell::Null)) => Ordering::Equal,
        (None, _) | (Some(Cell::Null), _) => Ordering::Less,
        (_, None) | (_, Some(Cell::Null)) => Ordering::Greater,
        (Some(a), Some(b)) => match (a, b) {
            (Cell::Int(x), Cell::Int(y)) => x.cmp(y),
            (Cell::UInt(x), Cell::UInt(y)) => x.cmp(y),
            (Cell::Int(x), Cell::UInt(y)) => (*x as i128).cmp(&(*y as i128)),
            (Cell::UInt(x), Cell::Int(y)) => (*x as i128).cmp(&(*y as i128)),
            (Cell::Float(x), Cell::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (Cell::Bool(x), Cell::Bool(y)) => x.cmp(y),
            (Cell::Blob(x), Cell::Blob(y)) => x.len().cmp(&y.len()),
            _ => a.display().cmp(&b.display()),
        },
    }
}

#[derive(Debug, Clone)]
pub struct BlobViewState {
    pub label: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct EditCellState {
    pub tab_id: u64,
    pub row: usize,
    pub col: usize,
    pub column_name: String,
    pub original_value: String,
    pub new_value: String,
}

/// Per-column entry in an [`InsertRowState`]. The user picks one mode and
/// the SQL builder turns it into a literal in the VALUES list (or omits the
/// column entirely when set to `Default`).
#[derive(Debug, Clone)]
pub enum InsertValue {
    /// User-typed literal — the builder quotes/escapes based on column type.
    Text(String),
    /// Emit `NULL` verbatim.
    Null,
    /// Omit this column from the column list (server picks the default /
    /// auto_increment / generated value).
    Default,
}

/// Modal state for the "Insert row" flow opened from an editable result tab.
/// Columns are loaded lazily via `list_columns`; until they land, `columns`
/// stays in `Loading` and the modal shows a spinner.
#[derive(Debug, Clone)]
pub struct InsertRowState {
    pub tab_id: u64,
    pub target: EditableTarget,
    pub columns: LoadState<Vec<ColumnInfo>>,
    /// One entry per column once `columns` is `Loaded`. Same indexing.
    pub values: Vec<InsertValue>,
}

impl InsertRowState {
    pub fn new(tab_id: u64, target: EditableTarget) -> Self {
        Self {
            tab_id,
            target,
            columns: LoadState::Loading,
            values: Vec::new(),
        }
    }

    /// Called by the app once `list_columns` returns. Picks a sensible
    /// default per column: `Default` for auto_increment / columns with a
    /// `default_value`, `Null` for nullable columns, otherwise an empty
    /// `Text`.
    pub fn seed_values(&mut self, columns: Vec<ColumnInfo>) {
        self.values = columns.iter().map(suggest_initial_value).collect();
        self.columns = LoadState::Loaded(columns);
    }
}

fn suggest_initial_value(col: &ColumnInfo) -> InsertValue {
    let has_default =
        col.default_value.is_some() || col.extra.to_ascii_lowercase().contains("auto_increment");
    if has_default {
        InsertValue::Default
    } else if col.nullable {
        InsertValue::Null
    } else {
        InsertValue::Text(String::new())
    }
}

#[derive(Default)]
pub struct ResultsState {
    pub tabs: Vec<ResultTab>,
    pub tab_seq: u64,
    pub blob_viewer: Option<BlobViewState>,
    pub edit_modal: Option<EditCellState>,
    pub insert_modal: Option<InsertRowState>,
}

/// Hard cap on the number of result tabs we keep around simultaneously. The
/// oldest tabs are evicted past this — applies only to tabs *we know about*
/// in `ResultsState`; the dock side mirrors the lifecycle via `on_close` so
/// the caller is responsible for keeping it in sync.
const MAX_RESULT_TABS: usize = 16;

impl ResultsState {
    pub fn next_tab_id(&mut self) -> u64 {
        self.tab_seq += 1;
        self.tab_seq
    }

    /// Push a freshly-built tab. Returns the id of an evicted tab, if any,
    /// so the caller can remove the matching dock tab.
    pub fn push(&mut self, tab: ResultTab) -> Option<u64> {
        self.tabs.push(tab);
        if self.tabs.len() > MAX_RESULT_TABS {
            Some(self.tabs.remove(0).tab_id)
        } else {
            None
        }
    }

    pub fn remove_by_id(&mut self, tab_id: u64) -> Option<ResultTab> {
        self.tabs
            .iter()
            .position(|t| t.tab_id == tab_id)
            .map(|i| self.tabs.remove(i))
    }

    pub fn find_by_id(&self, tab_id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.tab_id == tab_id)
    }
}

#[derive(Debug, Clone)]
pub struct EditRequest {
    pub tab_id: u64,
    pub row: usize,
    pub col: usize,
    pub sql: String,
    /// Pretty-printed SQL for the confirmation dialog.
    pub preview: String,
    /// What to display as the new cell value after a successful execute.
    pub new_value: String,
}

pub enum ResultsAction {
    CopyText(String),
    FetchMore {
        tab_id: u64,
        sql: String,
        offset: u64,
        limit: u64,
    },
    Export {
        tab_id: u64,
        format: ExportFormat,
    },
    OpenEdit {
        tab_id: u64,
        row: usize,
        col: usize,
        column_name: String,
        original_value: String,
    },
    ApplyEdit(EditRequest),
    /// User clicked `+ Add row`. App spawns `list_columns` and opens the
    /// insert modal once metadata lands.
    OpenInsert {
        tab_id: u64,
    },
    /// User triggered a single-row delete (right-click → Delete row…).
    /// `row` is the index into `tab.result.rows` (original, not display).
    OpenDeleteRow {
        tab_id: u64,
        row: usize,
    },
    /// User clicked `Delete N rows…`; the app reads `tab.selection` to
    /// build the WHERE clause.
    OpenBulkDelete {
        tab_id: u64,
    },
}

/// Render a single result tab's body (footer + grid). The dock owns the tab
/// strip and the close button; this function only paints the contents of one
/// leaf.
pub fn render_one(
    ui: &mut egui::Ui,
    state: &mut ResultsState,
    tab_id: u64,
    actions: &mut Vec<ResultsAction>,
) {
    let Some(idx) = state.find_by_id(tab_id) else {
        ui.label("Result tab closed");
        return;
    };
    // Split borrow so the table renderer can mutate the blob viewer without
    // re-borrowing `state`.
    let tabs = &mut state.tabs;
    let blob_viewer = &mut state.blob_viewer;
    let tab = &mut tabs[idx];

    // Top toolbar: only meaningful when the tab is editable (single-table,
    // PK detected). Hosts `+ Add row` and the bulk-delete button.
    if tab.editable.is_some() {
        let selection_count = tab.selection.len();
        egui::Panel::top(egui::Id::new(("results-toolbar", tab_id)))
            .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(6, 4)))
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("+ Add row").clicked() {
                        actions.push(ResultsAction::OpenInsert { tab_id });
                    }
                    if selection_count > 0 {
                        ui.separator();
                        let label = format!("Delete {selection_count} row(s)…");
                        let btn = egui::Button::new(
                            egui::RichText::new(label)
                                .color(egui::Color32::from_rgb(0xe5, 0x73, 0x73)),
                        );
                        if ui.add(btn).clicked() {
                            actions.push(ResultsAction::OpenBulkDelete { tab_id });
                        }
                    }
                });
            });
    }

    if tab.has_more {
        egui::Panel::bottom(egui::Id::new(("results-footer", tab_id)))
            .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(6, 4)))
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Showing {} rows · more available",
                            tab.result.rows.len()
                        ))
                        .weak()
                        .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(format!("Fetch next {}", tab.page_size)).clicked() {
                            actions.push(ResultsAction::FetchMore {
                                tab_id: tab.tab_id,
                                sql: tab.sql.clone(),
                                offset: tab.next_offset,
                                limit: tab.page_size,
                            });
                        }
                    });
                });
            });
    }
    render_table(ui, tab, blob_viewer, actions);
}

/// Title shown by the dock for a result tab. Includes the row count and a
/// trailing `+` when more pages are available.
pub fn tab_title(tab: &ResultTab) -> String {
    let suffix = if tab.has_more { "+" } else { "" };
    format!("{}  ({}{} rows)", tab.label, tab.result.rows.len(), suffix)
}

fn render_table(
    ui: &mut egui::Ui,
    tab: &mut ResultTab,
    blob_viewer: &mut Option<BlobViewState>,
    actions: &mut Vec<ResultsAction>,
) {
    if tab.result.columns.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(egui::RichText::new(format!("(no rows · {:.1?})", tab.result.elapsed)).weak());
        });
        return;
    }

    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let row_height = text_height + 4.0;
    let n_cols = tab.result.columns.len();
    // Show the row checkbox column only when the tab is editable AND the
    // PK was successfully detected — without a PK there's nothing to base
    // the DELETE WHERE on.
    let has_checkbox = tab
        .editable
        .as_ref()
        .map(|t| !t.pk_cols.is_empty())
        .unwrap_or(false);
    let table_min_width = 60.0 + (n_cols as f32) * 160.0 + if has_checkbox { 32.0 } else { 0.0 };
    let mut sort_click: Option<usize> = None;
    let mut edit_request: Option<(usize, usize)> = None;
    let mut delete_row_request: Option<usize> = None;
    let mut select_all_request: Option<bool> = None;
    let mut toggle_row_request: Option<(usize, bool)> = None;
    let total_rows = tab.result.rows.len();
    let all_selected = has_checkbox && total_rows > 0 && tab.selection.len() == total_rows;

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_min_width);

            let mut builder = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            if has_checkbox {
                builder = builder.column(Column::exact(28.0));
            }
            builder = builder.column(Column::auto().at_least(40.0).resizable(true));
            for _ in 0..n_cols {
                builder = builder.column(Column::initial(160.0).at_least(40.0).resizable(true));
            }

            builder
                .header(row_height + 4.0, |mut header| {
                    if has_checkbox {
                        header.col(|ui| {
                            let mut checked = all_selected;
                            if ui.checkbox(&mut checked, "").changed() {
                                select_all_request = Some(checked);
                            }
                        });
                    }
                    header.col(|ui| {
                        ui.label(egui::RichText::new("#").weak().small());
                    });
                    for (i, col) in tab.result.columns.iter().enumerate() {
                        header.col(|ui| {
                            let arrow = match tab.sort_by {
                                Some((c, SortOrder::Asc)) if c == i => "  ▲",
                                Some((c, SortOrder::Desc)) if c == i => "  ▼",
                                _ => "",
                            };
                            let resp = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{}{}", col.name, arrow)).strong(),
                                )
                                .selectable(false)
                                .truncate()
                                .sense(egui::Sense::click()),
                            );
                            resp.clone()
                                .on_hover_text(format!("{} · click to sort", col.type_name));
                            if resp.clicked() {
                                sort_click = Some(i);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, tab.order.len(), |mut row| {
                        let display_idx = row.index();
                        let original_idx = tab.order[display_idx];
                        let cells = &tab.result.rows[original_idx];

                        if has_checkbox {
                            row.col(|ui| {
                                let mut checked = tab.selection.contains(&original_idx);
                                if ui.checkbox(&mut checked, "").changed() {
                                    toggle_row_request = Some((original_idx, checked));
                                }
                            });
                        }
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new((original_idx + 1).to_string())
                                    .weak()
                                    .small(),
                            );
                        });
                        for (col_idx, cell) in cells.iter().enumerate() {
                            row.col(|ui| {
                                let action = render_cell(ui, cell, tab.editable.is_some());
                                match action {
                                    CellAction::None => {}
                                    CellAction::Copy => {
                                        actions.push(ResultsAction::CopyText(cell.display()));
                                    }
                                    CellAction::OpenBlob => {
                                        if let Cell::Blob(b) = cell {
                                            *blob_viewer = Some(BlobViewState {
                                                label: format!(
                                                    "{} · row {}",
                                                    tab.result.columns[col_idx].name,
                                                    original_idx + 1
                                                ),
                                                bytes: b.clone(),
                                            });
                                        }
                                    }
                                    CellAction::Edit => {
                                        edit_request = Some((original_idx, col_idx));
                                    }
                                    CellAction::DeleteRow => {
                                        delete_row_request = Some(original_idx);
                                    }
                                }
                            });
                        }
                    });
                });
        });

    if let Some(i) = sort_click {
        tab.cycle_sort(i);
    }

    if let Some(checked) = select_all_request {
        if checked {
            tab.selection = (0..total_rows).collect();
        } else {
            tab.selection.clear();
        }
    }
    if let Some((idx, checked)) = toggle_row_request {
        if checked {
            tab.selection.insert(idx);
        } else {
            tab.selection.remove(&idx);
        }
    }

    if let Some((row, col)) = edit_request {
        if let Some(seed) = build_edit_request(tab, row, col) {
            actions.push(ResultsAction::OpenEdit {
                tab_id: tab.tab_id,
                row,
                col,
                column_name: seed.column_name,
                original_value: seed.original_value,
            });
        }
    }

    if let Some(row) = delete_row_request {
        actions.push(ResultsAction::OpenDeleteRow {
            tab_id: tab.tab_id,
            row,
        });
    }
}

enum CellAction {
    None,
    Copy,
    OpenBlob,
    Edit,
    /// User picked "Delete row…" from the cell context menu — the row
    /// behind the clicked cell is the target.
    DeleteRow,
}

fn render_cell(ui: &mut egui::Ui, cell: &Cell, editable: bool) -> CellAction {
    let resp = match cell {
        Cell::Null => ui.add(
            egui::Label::new(egui::RichText::new("NULL").weak().italics())
                .selectable(false)
                .sense(egui::Sense::click()),
        ),
        Cell::Blob(b) => ui.add(
            egui::Label::new(
                egui::RichText::new(format!("<BLOB {} bytes>", b.len()))
                    .weak()
                    .italics(),
            )
            .selectable(false)
            .sense(egui::Sense::click()),
        ),
        Cell::Json(s) => {
            let one_line: String = s.chars().take(120).collect();
            let resp = ui.add(
                egui::Label::new(egui::RichText::new(one_line).monospace())
                    .selectable(false)
                    .sense(egui::Sense::click()),
            );
            resp.clone().on_hover_text(s);
            resp
        }
        _ => {
            let text = cell.display();
            let short: String = text.chars().take(160).collect();
            let resp = ui.add(
                egui::Label::new(short)
                    .selectable(false)
                    .sense(egui::Sense::click()),
            );
            if text.len() > 160 {
                resp.clone().on_hover_text(&text);
            }
            resp
        }
    };

    let mut out = CellAction::None;
    resp.context_menu(|ui| {
        if ui.button("Copy value").clicked() {
            out = CellAction::Copy;
            ui.close();
        }
        if matches!(cell, Cell::Blob(_)) && ui.button("View BLOB…").clicked() {
            out = CellAction::OpenBlob;
            ui.close();
        }
        if editable && !matches!(cell, Cell::Blob(_)) && ui.button("Edit cell…").clicked() {
            out = CellAction::Edit;
            ui.close();
        }
        if editable {
            ui.separator();
            if ui.button("Delete row…").clicked() {
                out = CellAction::DeleteRow;
                ui.close();
            }
        }
    });
    out
}

struct EditSeed {
    column_name: String,
    original_value: String,
}

fn build_edit_request(tab: &ResultTab, row: usize, col: usize) -> Option<EditSeed> {
    let cell = tab.result.rows.get(row)?.get(col)?;
    let column_name = tab.result.columns.get(col)?.name.clone();
    Some(EditSeed {
        column_name,
        original_value: cell.display(),
    })
}

// ===== BLOB viewer modal =====

pub fn render_blob_viewer(
    ctx: &egui::Context,
    state: &mut Option<BlobViewState>,
) -> Vec<ResultsAction> {
    let mut actions = Vec::new();
    let mut close = false;
    let Some(view) = state.as_ref() else {
        return actions;
    };
    egui::Modal::new(egui::Id::new("blob-viewer-modal")).show(ctx, |ui| {
        ui.set_min_width(640.0);
        ui.set_min_height(380.0);
        ui.heading(&view.label);
        ui.label(
            egui::RichText::new(format!("{} bytes", view.bytes.len()))
                .weak()
                .small(),
        );
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(hex_dump(&view.bytes)).monospace())
                        .selectable(true),
                );
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Copy hex").clicked() {
                actions.push(ResultsAction::CopyText(hex_dump(&view.bytes)));
            }
            if ui.button("Copy as text (lossy)").clicked() {
                actions.push(ResultsAction::CopyText(
                    String::from_utf8_lossy(&view.bytes).to_string(),
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    if close {
        *state = None;
    }
    actions
}

fn hex_dump(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 4096;
    let mut out = String::new();
    let limit = bytes.len().min(MAX_BYTES);
    for (i, chunk) in bytes[..limit].chunks(16).enumerate() {
        let offset = i * 16;
        let hex_part: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
        let mut hex_line = hex_part.join(" ");
        // Pad short last line so the ASCII column aligns.
        let target_len = 16 * 3 - 1;
        if hex_line.len() < target_len {
            hex_line.push_str(&" ".repeat(target_len - hex_line.len()));
        }
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (32..127).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{:08x}  {}  |{}|\n", offset, hex_line, ascii));
    }
    if bytes.len() > MAX_BYTES {
        out.push_str(&format!(
            "\n… ({} more bytes elided)\n",
            bytes.len() - MAX_BYTES
        ));
    }
    out
}

// ===== Edit cell modal =====

pub enum EditDialogChoice {
    None,
    Cancel,
    Submit,
}

pub fn render_edit_modal(ctx: &egui::Context, state: &mut EditCellState) -> EditDialogChoice {
    let mut choice = EditDialogChoice::None;
    egui::Modal::new(egui::Id::new("edit-cell-modal")).show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.heading(format!("Edit `{}`", state.column_name));
        ui.add_space(6.0);

        ui.label(egui::RichText::new("Current value:").weak().small());
        ui.add(
            egui::TextEdit::multiline(&mut state.original_value.clone())
                .interactive(false)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        ui.add_space(6.0);

        ui.label(egui::RichText::new("New value (or `NULL`):").weak().small());
        ui.add(
            egui::TextEdit::multiline(&mut state.new_value)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );

        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                choice = EditDialogChoice::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Continue…").clicked() {
                    choice = EditDialogChoice::Submit;
                }
            });
        });
    });
    choice
}

// ===== Export =====

pub fn export(tab: &ResultTab, format: ExportFormat) -> String {
    match format {
        ExportFormat::Csv => export_delimited(tab, ','),
        ExportFormat::Tsv => export_delimited(tab, '\t'),
        ExportFormat::Insert => export_insert(tab),
    }
}

fn export_delimited(tab: &ResultTab, sep: char) -> String {
    let mut out = String::new();
    let names: Vec<String> = tab
        .result
        .columns
        .iter()
        .map(|c| csv_escape(&c.name, sep))
        .collect();
    out.push_str(&names.join(&sep.to_string()));
    out.push('\n');
    for &row_idx in &tab.order {
        let row = &tab.result.rows[row_idx];
        let fields: Vec<String> = row
            .iter()
            .map(|c| match c {
                Cell::Null => String::new(),
                Cell::Blob(b) => csv_escape(&format!("<BLOB {} bytes>", b.len()), sep),
                _ => csv_escape(&c.display(), sep),
            })
            .collect();
        out.push_str(&fields.join(&sep.to_string()));
        out.push('\n');
    }
    out
}

fn csv_escape(s: &str, sep: char) -> String {
    let needs_quoting = s.contains(sep) || s.contains('"') || s.contains('\n') || s.contains('\r');
    if needs_quoting {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

fn export_insert(tab: &ResultTab) -> String {
    let mut out = String::new();
    let table = tab
        .editable
        .as_ref()
        .map(|t| {
            format!(
                "`{}`.`{}`",
                t.db.replace('`', "``"),
                t.table.replace('`', "``")
            )
        })
        .unwrap_or_else(|| "`<table>`".into());
    let cols: Vec<String> = tab
        .result
        .columns
        .iter()
        .map(|c| format!("`{}`", c.name.replace('`', "``")))
        .collect();
    let cols_joined = cols.join(", ");
    for &row_idx in &tab.order {
        let row = &tab.result.rows[row_idx];
        let values: Vec<String> = row.iter().map(sql_literal).collect();
        out.push_str(&format!(
            "INSERT INTO {} ({}) VALUES ({});\n",
            table,
            cols_joined,
            values.join(", ")
        ));
    }
    out
}

pub fn sql_literal(c: &Cell) -> String {
    match c {
        Cell::Null => "NULL".into(),
        Cell::Int(v) => v.to_string(),
        Cell::UInt(v) => v.to_string(),
        Cell::Float(v) => format!("{v}"),
        Cell::Bool(v) => (if *v { "1" } else { "0" }).into(),
        Cell::Text(s) | Cell::Json(s) => {
            format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
        }
        Cell::Blob(_) => "NULL /* BLOB */".into(),
    }
}

/// Build an UPDATE statement for a single cell change given a row and PK info.
pub fn build_update_sql(
    target: &EditableTarget,
    columns: &[ColumnMeta],
    row: &[Cell],
    col_idx: usize,
    new_value: &str,
) -> Result<String, String> {
    let col = columns
        .get(col_idx)
        .ok_or_else(|| "column out of range".to_string())?;
    let col_name = col
        .original_name
        .clone()
        .unwrap_or_else(|| col.name.clone());

    if target.pk_cols.is_empty() {
        return Err("no primary key available".into());
    }

    let set_value = if new_value.eq_ignore_ascii_case("NULL") {
        "NULL".to_string()
    } else {
        format!("'{}'", new_value.replace('\\', "\\\\").replace('\'', "''"))
    };

    let mut where_parts = Vec::with_capacity(target.pk_cols.len());
    for pk in &target.pk_cols {
        let pk_idx = columns
            .iter()
            .position(|c| c.original_name.as_deref() == Some(pk.as_str()) || c.name == *pk)
            .ok_or_else(|| format!("PK column `{pk}` not present in result"))?;
        let pk_cell = row
            .get(pk_idx)
            .ok_or_else(|| "row too short for PK".to_string())?;
        let literal = sql_literal(pk_cell);
        if literal.starts_with("NULL") {
            return Err(format!("PK column `{pk}` is NULL — cannot edit row"));
        }
        where_parts.push(format!("`{}` = {}", pk.replace('`', "``"), literal));
    }

    Ok(format!(
        "UPDATE `{}`.`{}` SET `{}` = {} WHERE {}",
        target.db.replace('`', "``"),
        target.table.replace('`', "``"),
        col_name.replace('`', "``"),
        set_value,
        where_parts.join(" AND ")
    ))
}

// ===== Insert row =====

pub enum InsertChoice {
    None,
    Cancel,
    Submit { sql: String },
}

/// Build the `INSERT INTO db.tbl (cols) VALUES (literals)` SQL for the given
/// values. Columns whose value is [`InsertValue::Default`] are omitted from
/// both lists so the server fills them in.
pub fn build_insert_sql(
    target: &EditableTarget,
    columns: &[ColumnInfo],
    values: &[InsertValue],
) -> Result<String, String> {
    if columns.len() != values.len() {
        return Err("column/value count mismatch".into());
    }
    let mut col_names = Vec::with_capacity(columns.len());
    let mut literals = Vec::with_capacity(columns.len());
    for (col, val) in columns.iter().zip(values.iter()) {
        match val {
            InsertValue::Default => continue,
            InsertValue::Null => {
                col_names.push(format!("`{}`", col.name.replace('`', "``")));
                literals.push("NULL".to_string());
            }
            InsertValue::Text(s) => {
                col_names.push(format!("`{}`", col.name.replace('`', "``")));
                literals.push(format_insert_literal(s, &col.data_type));
            }
        }
    }
    let db = target.db.replace('`', "``");
    let table = target.table.replace('`', "``");
    if col_names.is_empty() {
        // All columns left as DEFAULT: use the MySQL empty-row form.
        return Ok(format!("INSERT INTO `{db}`.`{table}` () VALUES ()"));
    }
    Ok(format!(
        "INSERT INTO `{}`.`{}` ({}) VALUES ({})",
        db,
        table,
        col_names.join(", "),
        literals.join(", ")
    ))
}

fn is_numeric_data_type(data_type: &str) -> bool {
    let lower = data_type.trim().to_ascii_lowercase();
    for prefix in [
        "tinyint",
        "smallint",
        "mediumint",
        "bigint",
        "int",
        "decimal",
        "numeric",
        "float",
        "double",
        "real",
        "bit",
    ] {
        if lower.starts_with(prefix) {
            return true;
        }
    }
    false
}

fn format_insert_literal(value: &str, data_type: &str) -> String {
    if is_numeric_data_type(data_type) && !value.is_empty() {
        // Ship the user-typed numeric verbatim; the server will reject
        // malformed input with a friendly error.
        value.to_string()
    } else {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
    }
}

pub fn render_insert_modal(ctx: &egui::Context, state: &mut InsertRowState) -> InsertChoice {
    let mut choice = InsertChoice::None;
    egui::Modal::new(egui::Id::new("insert-row-modal")).show(ctx, |ui| {
        ui.set_min_width(820.0);
        ui.heading(format!(
            "Insert into `{}`.`{}`",
            state.target.db, state.target.table
        ));
        ui.label(
            egui::RichText::new("Columns set to DEFAULT or auto_increment are left to the server.")
                .weak()
                .small(),
        );
        ui.add_space(10.0);

        match &state.columns {
            LoadState::NotLoaded | LoadState::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading columns…");
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        choice = InsertChoice::Cancel;
                    }
                });
                return;
            }
            LoadState::Error(e) => {
                ui.colored_label(egui::Color32::from_rgb(0xe5, 0x73, 0x73), e);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        choice = InsertChoice::Cancel;
                    }
                });
                return;
            }
            LoadState::Loaded(_) => {}
        };

        // Borrow `columns` once for the row layout; `values` is mutated by
        // index so we don't need a separate handle.
        let cols_clone: Vec<ColumnInfo> = match &state.columns {
            LoadState::Loaded(c) => c.clone(),
            _ => unreachable!(),
        };
        egui::ScrollArea::vertical()
            .id_salt("insert-modal-scroll")
            .max_height(480.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let last = cols_clone.len().saturating_sub(1);
                for (i, col) in cols_clone.iter().enumerate() {
                    render_insert_row(ui, i, col, &mut state.values[i]);
                    if i != last {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                    }
                }
            });

        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                choice = InsertChoice::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cols = match &state.columns {
                    LoadState::Loaded(c) => c.as_slice(),
                    _ => &[],
                };
                let preview = build_insert_sql(&state.target, cols, &state.values);
                let enabled = preview.is_ok();
                let btn = egui::Button::new("Insert…");
                if ui.add_enabled(enabled, btn).clicked() {
                    if let Ok(sql) = preview {
                        choice = InsertChoice::Submit { sql };
                    }
                }
            });
        });
    });
    choice
}

/// Single column block inside the Insert modal. Layout: top row has the
/// label (name + type / badges) on the left and the Value/NULL/DEFAULT
/// radios pinned to the right; bottom row is the value input that spans
/// the full modal width.
fn render_insert_row(ui: &mut egui::Ui, row_idx: usize, col: &ColumnInfo, value: &mut InsertValue) {
    ui.horizontal(|ui| {
        // Label cell: name on top line, type + badges on the muted second line.
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(&col.name).strong());
            ui.label(
                egui::RichText::new(badge_line(col))
                    .weak()
                    .small()
                    .monospace(),
            );
        });

        // Radios pinned to the right of the row, horizontal so the labels
        // stay on one line. The right-to-left layout reverses click order,
        // so we add Value last so it appears leftmost (visual reading order).
        let mut mode = mode_of(value);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.radio_value(&mut mode, InsertMode::Default, "DEFAULT");
            ui.add_space(4.0);
            ui.radio_value(&mut mode, InsertMode::Null, "NULL");
            ui.add_space(4.0);
            ui.radio_value(&mut mode, InsertMode::Value, "Value");
        });
        set_mode(value, mode);
    });

    // Input on its own row, full width. Non-interactive placeholder when the
    // mode isn't Value so the user can still see which column they're on
    // without the input collapsing.
    let id_salt = ("insert-modal-input", row_idx);
    match value {
        InsertValue::Text(s) => {
            ui.add(
                egui::TextEdit::singleline(s)
                    .id_salt(id_salt)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
        }
        InsertValue::Null => {
            let mut placeholder = String::from("NULL");
            ui.add(
                egui::TextEdit::singleline(&mut placeholder)
                    .id_salt(id_salt)
                    .interactive(false)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
        }
        InsertValue::Default => {
            let mut placeholder = String::from("(server default)");
            ui.add(
                egui::TextEdit::singleline(&mut placeholder)
                    .id_salt(id_salt)
                    .interactive(false)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
        }
    }
}

fn badge_line(col: &ColumnInfo) -> String {
    let mut hints = vec![col.data_type.clone()];
    if col.is_pk {
        hints.push("PK".to_string());
    }
    if !col.nullable {
        hints.push("NOT NULL".to_string());
    }
    if !col.extra.is_empty() {
        hints.push(col.extra.clone());
    }
    hints.join(" · ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InsertMode {
    Value,
    Null,
    Default,
}

fn mode_of(v: &InsertValue) -> InsertMode {
    match v {
        InsertValue::Text(_) => InsertMode::Value,
        InsertValue::Null => InsertMode::Null,
        InsertValue::Default => InsertMode::Default,
    }
}

fn set_mode(v: &mut InsertValue, mode: InsertMode) {
    match (&v, mode) {
        (InsertValue::Text(_), InsertMode::Value)
        | (InsertValue::Null, InsertMode::Null)
        | (InsertValue::Default, InsertMode::Default) => {}
        (_, InsertMode::Value) => *v = InsertValue::Text(String::new()),
        (_, InsertMode::Null) => *v = InsertValue::Null,
        (_, InsertMode::Default) => *v = InsertValue::Default,
    }
}

// ===== Delete rows =====

/// Build a `DELETE FROM db.tbl WHERE …` SQL statement for one or many rows
/// of an editable result tab. Branches on PK arity:
///   - single PK + 1 row  → `WHERE \`pk\` = literal`
///   - single PK + N rows → `WHERE \`pk\` IN (l1, l2, …)`
///   - composite PK + 1   → `WHERE (\`pk1\`, \`pk2\`) = (l1, l2)`
///   - composite PK + N   → `WHERE (\`pk1\`, \`pk2\`) IN ((…), (…))`
pub fn build_delete_sql(
    target: &EditableTarget,
    columns: &[ColumnMeta],
    rows: &[Vec<Cell>],
    row_indices: &[usize],
) -> Result<String, String> {
    if target.pk_cols.is_empty() {
        return Err("no primary key available".into());
    }
    if row_indices.is_empty() {
        return Err("no rows selected".into());
    }

    // Resolve PK column positions in the result so we can read literals
    // straight out of each row.
    let mut pk_col_indices = Vec::with_capacity(target.pk_cols.len());
    for pk in &target.pk_cols {
        let i = columns
            .iter()
            .position(|c| c.original_name.as_deref() == Some(pk.as_str()) || c.name == *pk)
            .ok_or_else(|| format!("PK column `{pk}` not present in result"))?;
        pk_col_indices.push(i);
    }

    // For each requested row, collect the tuple of PK literals.
    let mut tuples: Vec<Vec<String>> = Vec::with_capacity(row_indices.len());
    for &row_idx in row_indices {
        let row = rows
            .get(row_idx)
            .ok_or_else(|| format!("row index {row_idx} out of range"))?;
        let mut tuple = Vec::with_capacity(pk_col_indices.len());
        for (k, &col_idx) in pk_col_indices.iter().enumerate() {
            let cell = row
                .get(col_idx)
                .ok_or_else(|| "row too short for PK".to_string())?;
            let literal = sql_literal(cell);
            if literal.starts_with("NULL") {
                return Err(format!(
                    "PK column `{}` is NULL — cannot delete row",
                    target.pk_cols[k]
                ));
            }
            tuple.push(literal);
        }
        tuples.push(tuple);
    }

    let db = target.db.replace('`', "``");
    let table = target.table.replace('`', "``");

    if target.pk_cols.len() == 1 {
        let pk = target.pk_cols[0].replace('`', "``");
        if tuples.len() == 1 {
            return Ok(format!(
                "DELETE FROM `{db}`.`{table}` WHERE `{pk}` = {}",
                tuples[0][0]
            ));
        }
        let values: Vec<String> = tuples
            .into_iter()
            .map(|t| t.into_iter().next().unwrap())
            .collect();
        return Ok(format!(
            "DELETE FROM `{db}`.`{table}` WHERE `{pk}` IN ({})",
            values.join(", ")
        ));
    }

    let cols_list: Vec<String> = target
        .pk_cols
        .iter()
        .map(|p| format!("`{}`", p.replace('`', "``")))
        .collect();
    let tuple_strs: Vec<String> = tuples
        .iter()
        .map(|t| format!("({})", t.join(", ")))
        .collect();
    if tuple_strs.len() == 1 {
        Ok(format!(
            "DELETE FROM `{db}`.`{table}` WHERE ({}) = {}",
            cols_list.join(", "),
            tuple_strs[0]
        ))
    } else {
        Ok(format!(
            "DELETE FROM `{db}`.`{table}` WHERE ({}) IN ({})",
            cols_list.join(", "),
            tuple_strs.join(", ")
        ))
    }
}
