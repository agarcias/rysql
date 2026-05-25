//! Results pane: tabs of result sets rendered with `egui_extras::TableBuilder`.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rysql_db::{Cell, QueryResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub struct ResultTab {
    pub label: String,
    pub result: QueryResult,
    /// Index permutation: `order[i]` is the original row index displayed at row `i`.
    pub order: Vec<usize>,
    pub sort_by: Option<(usize, SortOrder)>,
}

impl ResultTab {
    pub fn new(label: String, result: QueryResult) -> Self {
        let order = (0..result.rows.len()).collect();
        Self {
            label,
            result,
            order,
            sort_by: None,
        }
    }

    fn apply_sort(&mut self) {
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
            _ => a.display().cmp(&b.display()),
        },
    }
}

#[derive(Default)]
pub struct ResultsState {
    pub tabs: Vec<ResultTab>,
    pub active: usize,
}

impl ResultsState {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
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
}

pub enum ResultsAction {
    SelectTab(usize),
    CloseTab(usize),
    CopyText(String),
}

pub fn render(ui: &mut egui::Ui, state: &mut ResultsState) -> Vec<ResultsAction> {
    let mut actions = Vec::new();
    if state.tabs.is_empty() {
        return actions;
    }

    egui::Panel::top("results-tab-strip")
        .frame(egui::Frame::default().inner_margin(egui::Margin::symmetric(6, 4)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let mut to_close: Option<usize> = None;
                for (idx, tab) in state.tabs.iter().enumerate() {
                    let is_active = idx == state.active;
                    let label = format!("{}  ({} rows)", tab.label, tab.result.rows.len());
                    if ui.selectable_label(is_active, label).clicked() && !is_active {
                        actions.push(ResultsAction::SelectTab(idx));
                    }
                    if ui.small_button("×").clicked() {
                        to_close = Some(idx);
                    }
                    ui.separator();
                }
                if let Some(i) = to_close {
                    actions.push(ResultsAction::CloseTab(i));
                }
            });
        });

    if let Some(tab) = state.tabs.get_mut(state.active) {
        render_table(ui, tab, &mut actions);
    }

    actions
}

fn render_table(ui: &mut egui::Ui, tab: &mut ResultTab, actions: &mut Vec<ResultsAction>) {
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
    // Reserve roughly 60 px for the row-number column, then 160 per data column.
    // This is the natural width of the table; the outer ScrollArea pans
    // horizontally when it exceeds the visible viewport.
    let table_min_width = 60.0 + (n_cols as f32) * 160.0;
    let mut sort_click: Option<usize> = None;

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_min_width);

            let mut builder = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().at_least(40.0).resizable(true)); // row number
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
                        for cell in cells {
                            row.col(|ui| {
                                render_cell(ui, cell, actions);
                            });
                        }
                    });
                });
        });

    if let Some(i) = sort_click {
        tab.cycle_sort(i);
    }
}

fn render_cell(ui: &mut egui::Ui, cell: &Cell, actions: &mut Vec<ResultsAction>) {
    let resp = match cell {
        Cell::Null => ui.add(
            egui::Label::new(egui::RichText::new("NULL").weak().italics())
                .selectable(false)
                .sense(egui::Sense::click()),
        ),
        Cell::Blob(n) => ui.add(
            egui::Label::new(
                egui::RichText::new(format!("<BLOB {n} bytes>"))
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

    resp.context_menu(|ui| {
        let value = cell.display();
        if ui.button("Copy value").clicked() {
            actions.push(ResultsAction::CopyText(value));
            ui.close();
        }
    });
}
