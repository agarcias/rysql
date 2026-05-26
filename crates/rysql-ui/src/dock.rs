//! `egui_dock` glue: the [`DockTab`] enum is the discriminator for every tab
//! that lives in the central dock area; [`AppViewer`] is a thin
//! [`egui_dock::TabViewer`] adapter that delegates rendering to the
//! per-module renderers ([`crate::editor`], [`crate::results`],
//! [`crate::object_view`]) and collects [`DockAction`] / [`ResultsAction`]
//! intents that the caller applies after the dock has been rendered.

use std::collections::HashMap;

use eframe::egui;
use egui_dock::{tab_viewer::OnCloseResponse, TabViewer};
use rysql_db::ObjectKind;
use rysql_sql::Highlighter;

use crate::editor::{self, EditorContext, EditorState};
use crate::object_view::{self, ObjectAction, ObjectViewState};
use crate::results::{self, ExportFormat, ResultsAction, ResultsState};
use crate::state::ObjectKey;

/// Discriminator for a tab living in the central [`egui_dock::DockState`].
///
/// The tab itself carries only an id; the actual state lives in `RysqlApp`
/// (e.g. [`EditorState::buffers`] indexed by [`crate::editor::Buffer::id`]),
/// keeping the dock state small and easy to serialize later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DockTab {
    /// SQL editor tab backed by the buffer with this stable id.
    SqlEditor { buffer_id: u64 },
    /// Result set tab backed by [`crate::results::ResultsState::tabs`] keyed
    /// by `tab_id`.
    Results { tab_id: u64 },
    /// Schema-object inspector backed by an
    /// [`crate::object_view::ObjectViewState`] keyed by an [`ObjectKey`].
    Object { key: ObjectKey },
}

/// Intent collected during a dock render frame, applied by `RysqlApp` once
/// the borrow on app state held by [`AppViewer`] has been released.
#[derive(Debug, Clone)]
pub enum DockAction {
    /// The user clicked the close button on an editor tab; free the buffer.
    CloseEditorBuffer(u64),
    /// The user closed a result tab; free the result set.
    CloseResultsTab(u64),
    /// The user closed an object inspector tab; free its
    /// [`ObjectViewState`] and any embedded data result.
    CloseObjectView(ObjectKey),
    /// Loader intent forwarded from a rendered object view this frame.
    ObjectRequest {
        key: ObjectKey,
        action: ObjectAction,
    },
    /// The editor for this buffer is the one with keyboard focus this frame.
    FocusedEditor(u64),
}

/// `egui_dock::TabViewer` implementation. Owns short-lived mutable borrows
/// of the underlying app state needed to render each tab variant.
pub struct AppViewer<'a> {
    pub editor: &'a mut EditorState,
    pub results: &'a mut ResultsState,
    pub objects: &'a mut HashMap<ObjectKey, ObjectViewState>,
    pub source_highlighter: &'a mut Highlighter,
    pub schema_names: &'a [String],
    pub actions: Vec<DockAction>,
    pub results_actions: Vec<ResultsAction>,
}

impl<'a> AppViewer<'a> {
    pub fn new(
        editor: &'a mut EditorState,
        results: &'a mut ResultsState,
        objects: &'a mut HashMap<ObjectKey, ObjectViewState>,
        source_highlighter: &'a mut Highlighter,
        schema_names: &'a [String],
    ) -> Self {
        Self {
            editor,
            results,
            objects,
            source_highlighter,
            schema_names,
            actions: Vec::new(),
            results_actions: Vec::new(),
        }
    }
}

impl TabViewer for AppViewer<'_> {
    type Tab = DockTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            DockTab::SqlEditor { buffer_id } => {
                let label = self
                    .editor
                    .buffer_by_id(*buffer_id)
                    .map(|b| {
                        if b.dirty {
                            format!("● {}", b.name)
                        } else {
                            b.name.clone()
                        }
                    })
                    .unwrap_or_else(|| format!("query-{buffer_id}"));
                label.into()
            }
            DockTab::Results { tab_id } => {
                let label = self
                    .results
                    .find_by_id(*tab_id)
                    .and_then(|i| self.results.tabs.get(i))
                    .map(results::tab_title)
                    .unwrap_or_else(|| format!("results #{tab_id}"));
                label.into()
            }
            DockTab::Object { key } => format!("{}.{}", key.db, key.name).into(),
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        match tab {
            DockTab::SqlEditor { buffer_id } => egui::Id::new(("dock-tab-editor", *buffer_id)),
            DockTab::Results { tab_id } => egui::Id::new(("dock-tab-results", *tab_id)),
            DockTab::Object { key } => egui::Id::new(("dock-tab-object", key)),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            DockTab::SqlEditor { buffer_id } => {
                let ctx = EditorContext {
                    schema_names: self.schema_names,
                };
                let focused = editor::render_one(ui, self.editor, *buffer_id, ctx);
                if focused {
                    self.actions.push(DockAction::FocusedEditor(*buffer_id));
                }
            }
            DockTab::Results { tab_id } => {
                results::render_one(ui, self.results, *tab_id, &mut self.results_actions);
            }
            DockTab::Object { key } => {
                if let Some(state) = self.objects.get_mut(key) {
                    let intents = object_view::render(
                        ui,
                        state,
                        self.results,
                        self.source_highlighter,
                        &mut self.results_actions,
                    );
                    for action in intents {
                        self.actions.push(DockAction::ObjectRequest {
                            key: key.clone(),
                            action,
                        });
                    }
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "Object `{}.{}` is no longer available.",
                            key.db, key.name
                        ))
                        .weak(),
                    );
                }
            }
        }
    }

    fn on_tab_button(&mut self, tab: &mut Self::Tab, response: &egui::Response) {
        match tab {
            DockTab::Object { key } => {
                let kind = match key.kind {
                    ObjectKind::Table => "Table",
                    ObjectKind::View => "View",
                    ObjectKind::Procedure => "Procedure",
                    ObjectKind::Function => "Function",
                    ObjectKind::Trigger => "Trigger",
                    ObjectKind::Event => "Event",
                };
                response
                    .clone()
                    .on_hover_text(format!("{kind} · `{}`.`{}`", key.db, key.name));
            }
            DockTab::Results { tab_id } => {
                if let Some(i) = self.results.find_by_id(*tab_id) {
                    if let Some(tab) = self.results.tabs.get(i) {
                        response.clone().on_hover_text(&tab.sql);
                    }
                }
            }
            DockTab::SqlEditor { buffer_id } => {
                if let Some(buf) = self.editor.buffer_by_id(*buffer_id) {
                    if let Some(path) = buf.path.as_ref() {
                        response.clone().on_hover_text(path.display().to_string());
                    }
                }
            }
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab, _path: egui_dock::NodePath) {
        if let DockTab::Results { tab_id } = tab {
            ui.label(egui::RichText::new("Export").weak().small());
            for (label, format) in [
                ("Copy as CSV", ExportFormat::Csv),
                ("Copy as TSV", ExportFormat::Tsv),
                ("Copy as INSERT", ExportFormat::Insert),
            ] {
                if ui.button(label).clicked() {
                    self.results_actions.push(ResultsAction::Export {
                        tab_id: *tab_id,
                        format,
                    });
                    ui.close();
                }
            }
        }
    }

    fn on_close(&mut self, tab: &mut Self::Tab) -> OnCloseResponse {
        match tab {
            DockTab::SqlEditor { buffer_id } => {
                self.actions.push(DockAction::CloseEditorBuffer(*buffer_id));
            }
            DockTab::Results { tab_id } => {
                self.actions.push(DockAction::CloseResultsTab(*tab_id));
            }
            DockTab::Object { key } => {
                self.actions.push(DockAction::CloseObjectView(key.clone()));
            }
        }
        OnCloseResponse::Close
    }
}
