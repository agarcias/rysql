//! Modal for adding (and later modifying) a column on a table. The state
//! is kept on `RysqlApp::column_edit_modal`; [`render_column_edit_modal`]
//! drives the per-frame rendering and returns a [`ColumnEditChoice`] for
//! the caller to act on.
//!
//! Day 5 scope: `ColumnEditMode::Add`. Day 6 will extend with `Modify`.

use eframe::egui;

use crate::state::ObjectKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnEditMode {
    /// `ALTER TABLE … ADD COLUMN …`.
    Add,
}

/// How the user wants the column's `DEFAULT` to be emitted in the ALTER
/// statement. Mutually exclusive with `auto_increment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDefault {
    /// Don't emit a DEFAULT clause — the server picks its own.
    None,
    /// Emit `DEFAULT NULL` (only meaningful when the column is nullable).
    Null,
    /// Emit `DEFAULT '<value>'` — the user-typed literal is quoted and
    /// escaped. Numeric coercion happens server-side.
    Value,
}

#[derive(Debug, Clone)]
pub struct ColumnEditState {
    pub key: ObjectKey,
    pub mode: ColumnEditMode,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_mode: ColumnDefault,
    pub default_value: String,
    pub auto_increment: bool,
    pub comment: String,
}

impl ColumnEditState {
    pub fn new_add(key: ObjectKey) -> Self {
        Self {
            key,
            mode: ColumnEditMode::Add,
            name: String::new(),
            data_type: String::new(),
            nullable: true,
            default_mode: ColumnDefault::None,
            default_value: String::new(),
            auto_increment: false,
            comment: String::new(),
        }
    }
}

pub enum ColumnEditChoice {
    None,
    Cancel,
    Submit { sql: String },
}

/// Build the `ALTER TABLE … ADD COLUMN …` SQL for the current state. The
/// caller is responsible for pushing it through the confirm modal.
pub fn build_add_column_sql(state: &ColumnEditState) -> Result<String, String> {
    let name = state.name.trim();
    let data_type = state.data_type.trim();
    if name.is_empty() {
        return Err("Column name is required".into());
    }
    if data_type.is_empty() {
        return Err("Type is required".into());
    }

    let db = state.key.db.replace('`', "``");
    let table = state.key.name.replace('`', "``");
    let col = name.replace('`', "``");

    let mut sql = format!("ALTER TABLE `{db}`.`{table}` ADD COLUMN `{col}` {data_type}");
    sql.push_str(if state.nullable { " NULL" } else { " NOT NULL" });

    // DEFAULT and AUTO_INCREMENT are conceptually mutually exclusive:
    // AUTO_INCREMENT is its own server-managed default. If the user ticked
    // both, AUTO_INCREMENT wins.
    if state.auto_increment {
        sql.push_str(" AUTO_INCREMENT");
    } else {
        match state.default_mode {
            ColumnDefault::None => {}
            ColumnDefault::Null => {
                if state.nullable {
                    sql.push_str(" DEFAULT NULL");
                }
            }
            ColumnDefault::Value => {
                let escaped = state
                    .default_value
                    .replace('\\', "\\\\")
                    .replace('\'', "''");
                sql.push_str(&format!(" DEFAULT '{escaped}'"));
            }
        }
    }

    let comment = state.comment.trim();
    if !comment.is_empty() {
        let escaped = comment.replace('\\', "\\\\").replace('\'', "''");
        sql.push_str(&format!(" COMMENT '{escaped}'"));
    }

    Ok(sql)
}

/// Build the `ALTER TABLE … DROP COLUMN …` SQL for the given column.
pub fn build_drop_column_sql(key: &ObjectKey, col_name: &str) -> String {
    format!(
        "ALTER TABLE `{}`.`{}` DROP COLUMN `{}`",
        key.db.replace('`', "``"),
        key.name.replace('`', "``"),
        col_name.replace('`', "``"),
    )
}

pub fn render_column_edit_modal(
    ctx: &egui::Context,
    state: &mut ColumnEditState,
) -> ColumnEditChoice {
    let mut choice = ColumnEditChoice::None;
    let title = match state.mode {
        ColumnEditMode::Add => format!("Add column to `{}`.`{}`", state.key.db, state.key.name),
    };
    egui::Modal::new(egui::Id::new("column-edit-modal")).show(ctx, |ui| {
        ui.set_min_width(720.0);
        ui.heading(title);
        ui.label(
            egui::RichText::new(
                "Type is free-form — write whatever MariaDB/MySQL accepts \
                 (varchar(64), decimal(10,2), enum('a','b'), …).",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);

        egui::Grid::new("column-edit-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Name").strong());
                ui.add(
                    egui::TextEdit::singleline(&mut state.name)
                        .desired_width(480.0)
                        .font(egui::TextStyle::Monospace),
                );
                ui.end_row();

                ui.label(egui::RichText::new("Type").strong());
                ui.add(
                    egui::TextEdit::singleline(&mut state.data_type)
                        .desired_width(480.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("varchar(64)"),
                );
                ui.end_row();

                ui.label(egui::RichText::new("Nullable").strong());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.nullable, "Allow NULL");
                    ui.add_space(16.0);
                    ui.checkbox(&mut state.auto_increment, "AUTO_INCREMENT");
                });
                ui.end_row();

                ui.label(egui::RichText::new("Default").strong());
                ui.horizontal(|ui| {
                    ui.radio_value(&mut state.default_mode, ColumnDefault::None, "None");
                    ui.add_space(8.0);
                    ui.add_enabled_ui(state.nullable, |ui| {
                        ui.radio_value(&mut state.default_mode, ColumnDefault::Null, "NULL");
                    });
                    ui.add_space(8.0);
                    ui.radio_value(&mut state.default_mode, ColumnDefault::Value, "Value");
                });
                ui.end_row();

                ui.label("");
                ui.add_enabled_ui(state.default_mode == ColumnDefault::Value, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.default_value)
                            .desired_width(480.0)
                            .font(egui::TextStyle::Monospace),
                    );
                });
                ui.end_row();

                ui.label(egui::RichText::new("Comment").strong());
                ui.add(egui::TextEdit::singleline(&mut state.comment).desired_width(480.0));
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Preview SQL").weak().small());
        let preview = build_add_column_sql(state);
        let mut preview_text = match &preview {
            Ok(s) => s.clone(),
            Err(e) => format!("({e})"),
        };
        egui::ScrollArea::vertical()
            .id_salt("column-edit-preview-scroll")
            .max_height(180.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut preview_text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .interactive(false)
                        .font(egui::TextStyle::Monospace),
                );
            });

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                choice = ColumnEditChoice::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = preview.is_ok();
                let label = match state.mode {
                    ColumnEditMode::Add => "Add…",
                };
                let btn = egui::Button::new(label);
                if ui.add_enabled(enabled, btn).clicked() {
                    if let Ok(sql) = preview {
                        choice = ColumnEditChoice::Submit { sql };
                    }
                }
            });
        });
    });
    choice
}
