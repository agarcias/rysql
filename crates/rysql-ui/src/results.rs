//! Results pane: tabs of result sets rendered with `egui_extras::TableBuilder`.
//!
//! Phase 4b features: pagination (auto-LIMIT + fetch next), BLOB viewer,
//! export to clipboard (CSV / TSV / INSERT), and edit-in-place by primary key.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rysql_db::{Cell, ColumnMeta, QueryResult};

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

#[derive(Default)]
pub struct ResultsState {
    pub tabs: Vec<ResultTab>,
    pub active: usize,
    pub tab_seq: u64,
    pub blob_viewer: Option<BlobViewState>,
    pub edit_modal: Option<EditCellState>,
}

impl ResultsState {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn next_tab_id(&mut self) -> u64 {
        self.tab_seq += 1;
        self.tab_seq
    }

    pub fn push(&mut self, tab: ResultTab) {
        const MAX: usize = 16;
        self.tabs.push(tab);
        if self.tabs.len() > MAX {
            self.tabs.remove(0);
        }
        self.active = self.tabs.len() - 1;
    }

    pub fn close(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() && self.active > 0 {
            self.active = self.tabs.len().saturating_sub(1);
        }
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
    SelectTab(usize),
    CloseTab(usize),
    CopyText(String),
    FetchMore {
        tab_id: u64,
        sql: String,
        offset: u64,
        limit: u64,
    },
    Export {
        tab_idx: usize,
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
}

pub fn render(ui: &mut egui::Ui, state: &mut ResultsState) -> Vec<ResultsAction> {
    let mut actions = Vec::new();
    if state.tabs.is_empty() {
        return actions;
    }

    let active_idx = state.active;
    egui::Panel::top("results-tab-strip")
        .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(6, 4)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let mut to_close: Option<usize> = None;
                let mut to_export: Option<(usize, ExportFormat)> = None;
                for (idx, tab) in state.tabs.iter().enumerate() {
                    let is_active = idx == active_idx;
                    let suffix = if tab.has_more { "+" } else { "" };
                    let label =
                        format!("{}  ({}{} rows)", tab.label, tab.result.rows.len(), suffix);
                    let resp = ui.selectable_label(is_active, label);
                    if resp.clicked() && !is_active {
                        actions.push(ResultsAction::SelectTab(idx));
                    }
                    resp.context_menu(|ui| {
                        ui.label(egui::RichText::new("Export").weak().small());
                        if ui.button("Copy as CSV").clicked() {
                            to_export = Some((idx, ExportFormat::Csv));
                            ui.close();
                        }
                        if ui.button("Copy as TSV").clicked() {
                            to_export = Some((idx, ExportFormat::Tsv));
                            ui.close();
                        }
                        if ui.button("Copy as INSERT").clicked() {
                            to_export = Some((idx, ExportFormat::Insert));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Close").clicked() {
                            to_close = Some(idx);
                            ui.close();
                        }
                    });
                    if ui.small_button("×").clicked() {
                        to_close = Some(idx);
                    }
                    ui.separator();
                }
                if let Some(i) = to_close {
                    actions.push(ResultsAction::CloseTab(i));
                }
                if let Some((i, fmt)) = to_export {
                    actions.push(ResultsAction::Export {
                        tab_idx: i,
                        format: fmt,
                    });
                }
            });
        });

    if let Some(tab) = state.tabs.get_mut(active_idx) {
        if tab.has_more {
            egui::Panel::bottom("results-footer")
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
        render_table(ui, tab, active_idx, &mut state.blob_viewer, &mut actions);
    }

    actions
}

fn render_table(
    ui: &mut egui::Ui,
    tab: &mut ResultTab,
    tab_idx: usize,
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
    let table_min_width = 60.0 + (n_cols as f32) * 160.0;
    let mut sort_click: Option<usize> = None;
    let mut edit_request: Option<(usize, usize)> = None;

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_min_width);

            let mut builder = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().at_least(40.0).resizable(true));
            for _ in 0..n_cols {
                builder = builder.column(Column::initial(160.0).at_least(40.0).resizable(true));
            }

            builder
                .header(row_height + 4.0, |mut header| {
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
                                }
                            });
                        }
                    });
                });
        });

    if let Some(i) = sort_click {
        tab.cycle_sort(i);
    }

    let _ = tab_idx;
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
}

enum CellAction {
    None,
    Copy,
    OpenBlob,
    Edit,
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
