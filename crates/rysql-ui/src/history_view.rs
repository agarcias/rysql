//! Query-history browser: searchable modal over [`HistoryEntry`]s.

use eframe::egui;
use rysql_core::HistoryEntry;

#[derive(Default)]
pub struct HistoryView {
    pub open: bool,
    pub entries: Vec<HistoryEntry>,
    pub query: String,
}

pub enum HistoryAction {
    None,
    Close,
    LoadIntoEditor(String),
    Clear,
}

pub fn render(ctx: &egui::Context, state: &mut HistoryView) -> HistoryAction {
    let mut action = HistoryAction::None;
    if !state.open {
        return action;
    }

    egui::Modal::new(egui::Id::new("history-modal")).show(ctx, |ui| {
        ui.set_min_width(780.0);
        ui.set_min_height(420.0);
        ui.horizontal(|ui| {
            ui.heading("Query history");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    action = HistoryAction::Close;
                }
                if ui.button("Clear all…").clicked() {
                    action = HistoryAction::Clear;
                }
            });
        });
        ui.label(
            egui::RichText::new(format!("{} entries", state.entries.len()))
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .hint_text("substring (case-insensitive)")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.separator();

        let needle = state.query.to_ascii_lowercase();
        let filtered: Vec<(usize, &HistoryEntry)> = state
            .entries
            .iter()
            .enumerate()
            .rev() // newest first
            .filter(|(_, e)| {
                if needle.is_empty() {
                    true
                } else {
                    e.sql.to_ascii_lowercase().contains(&needle)
                        || e.profile.to_ascii_lowercase().contains(&needle)
                        || e.summary.to_ascii_lowercase().contains(&needle)
                }
            })
            .collect();

        egui::ScrollArea::vertical()
            .max_height(360.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (_, entry) in filtered {
                    history_row(ui, entry, &mut action);
                    ui.separator();
                }
            });
    });

    action
}

fn history_row(ui: &mut egui::Ui, entry: &HistoryEntry, action: &mut HistoryAction) {
    ui.horizontal(|ui| {
        let dot = if entry.success {
            egui::RichText::new("●").color(egui::Color32::from_rgb(0x4c, 0xaf, 0x50))
        } else {
            egui::RichText::new("●").color(egui::Color32::from_rgb(0xe5, 0x73, 0x73))
        };
        ui.label(dot);
        ui.label(
            egui::RichText::new(&entry.timestamp)
                .weak()
                .small()
                .monospace(),
        );
        ui.label(
            egui::RichText::new(format!("· {}", entry.profile))
                .weak()
                .small(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Load").clicked() {
                *action = HistoryAction::LoadIntoEditor(entry.sql.clone());
            }
            if ui.button("Copy SQL").clicked() {
                ui.ctx().copy_text(entry.sql.clone());
            }
        });
    });
    let one_line = entry.sql.lines().next().unwrap_or("").trim();
    let display: String = one_line.chars().take(200).collect();
    let suffix = if one_line.len() > display.len() || entry.sql.lines().count() > 1 {
        "…"
    } else {
        ""
    };
    ui.label(egui::RichText::new(format!("{display}{suffix}")).monospace());
    if !entry.summary.is_empty() {
        ui.label(egui::RichText::new(&entry.summary).weak().small().italics());
    }
}
