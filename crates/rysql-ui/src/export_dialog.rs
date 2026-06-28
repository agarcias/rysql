//! Modal for exporting a whole database. Mirrors the `column_dialog` pattern:
//! the state lives on `RysqlApp::export_modal`, [`render_export_modal`] drives
//! the per-frame rendering and returns an [`ExportChoice`] for the caller.
//!
//! The object/data toggles are kept on a [`rysql_db::SqlDumpOptions`] so the
//! dialog state maps straight onto what the export engine consumes.

use eframe::egui;
use rysql_db::SqlDumpOptions;

/// Which kind of output the user wants to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// One re-importable `.sql` dump (structure + optional data + objects).
    Sql,
    /// One `<table>.csv` file per table (data only) in a chosen folder.
    Csv,
}

#[derive(Debug, Clone)]
pub struct ExportState {
    pub db: String,
    pub format: OutputFormat,
    pub opts: SqlDumpOptions,
}

impl ExportState {
    pub fn new(db: String) -> Self {
        Self {
            db,
            format: OutputFormat::Sql,
            opts: SqlDumpOptions::default(),
        }
    }
}

pub enum ExportChoice {
    /// Modal still open; keep `ExportState` and render again next frame.
    None,
    /// User dismissed the dialog.
    Cancel,
    /// User confirmed; the caller picks a destination and runs the export.
    Run,
}

pub fn render_export_modal(ctx: &egui::Context, state: &mut ExportState) -> ExportChoice {
    let mut choice = ExportChoice::None;
    egui::Modal::new(egui::Id::new("export-database-modal")).show(ctx, |ui| {
        ui.set_min_width(460.0);
        ui.heading(format!("Export database `{}`", state.db));
        ui.add_space(8.0);

        ui.label(egui::RichText::new("Format").strong());
        ui.horizontal(|ui| {
            ui.radio_value(&mut state.format, OutputFormat::Sql, "SQL dump (.sql)");
            ui.add_space(12.0);
            ui.radio_value(&mut state.format, OutputFormat::Csv, "CSV per table");
        });
        ui.add_space(8.0);

        match state.format {
            OutputFormat::Sql => render_sql_options(ui, &mut state.opts),
            OutputFormat::Csv => {
                ui.label(
                    egui::RichText::new(
                        "Writes one <table>.csv file (data only) per table into a folder \
                         you choose. Views, routines, triggers and events are not included.",
                    )
                    .weak(),
                );
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                choice = ExportChoice::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = match state.format {
                    OutputFormat::Sql => any_object_selected(&state.opts),
                    OutputFormat::Csv => true,
                };
                let btn = egui::Button::new("Export…");
                let resp = ui.add_enabled(enabled, btn);
                let resp = if enabled {
                    resp
                } else {
                    resp.on_hover_text("Select at least one object type to export")
                };
                if resp.clicked() {
                    choice = ExportChoice::Run;
                }
            });
        });
    });
    choice
}

fn render_sql_options(ui: &mut egui::Ui, opts: &mut SqlDumpOptions) {
    ui.label(egui::RichText::new("Include").strong());
    egui::Grid::new("export-objects-grid")
        .num_columns(2)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            ui.checkbox(&mut opts.tables, "Tables");
            ui.checkbox(&mut opts.views, "Views");
            ui.end_row();
            ui.checkbox(&mut opts.procedures, "Procedures");
            ui.checkbox(&mut opts.functions, "Functions");
            ui.end_row();
            ui.checkbox(&mut opts.triggers, "Triggers");
            ui.checkbox(&mut opts.events, "Events");
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.add_enabled_ui(opts.tables, |ui| {
        ui.checkbox(&mut opts.data, "Table data (INSERT statements)");
    });

    ui.add_space(8.0);
    ui.label(egui::RichText::new("Options").strong());
    ui.checkbox(
        &mut opts.drop_if_exists,
        "DROP … IF EXISTS before each CREATE",
    );
    ui.checkbox(
        &mut opts.create_database,
        "CREATE DATABASE / USE statements",
    );
    ui.checkbox(
        &mut opts.portability_header,
        "Portability header (SET NAMES, FOREIGN_KEY_CHECKS=0, SQL_MODE)",
    );
    ui.add_enabled_ui(opts.tables && opts.data, |ui| {
        ui.checkbox(
            &mut opts.extended_insert,
            "Extended INSERT (group rows per statement)",
        );
    });
    ui.checkbox(&mut opts.omit_definer, "Omit DEFINER clauses");
    ui.checkbox(&mut opts.comment_header, "Comment header");
}

/// At least one object category must be on, otherwise the dump would be empty.
fn any_object_selected(opts: &SqlDumpOptions) -> bool {
    opts.tables || opts.views || opts.procedures || opts.functions || opts.triggers || opts.events
}
