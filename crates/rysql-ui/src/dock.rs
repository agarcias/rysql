//! `egui_dock` glue: the [`DockTab`] enum is the discriminator for every tab
//! that lives in the central dock area; [`AppViewer`] is a thin
//! [`egui_dock::TabViewer`] adapter that delegates rendering to the
//! per-module renderers ([`crate::editor`], [`crate::results`]) and collects
//! [`DockAction`] / [`ResultsAction`] intents that the caller applies after
//! the dock has been rendered.
//!
//! The `Object` tab variant lands on Day 3 per `docs/dock-layout-plan.md`.

use eframe::egui;
use egui_dock::{tab_viewer::OnCloseResponse, TabViewer};

use crate::editor::{self, EditorContext, EditorState};
use crate::results::{self, ExportFormat, ResultsAction, ResultsState};

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
}

/// Intent collected during a dock render frame, applied by `RysqlApp` once
/// the borrow on app state held by [`AppViewer`] has been released.
#[derive(Debug, Clone)]
pub enum DockAction {
    /// The user clicked the close button on an editor tab; free the buffer.
    CloseEditorBuffer(u64),
    /// The user closed a result tab; free the result set.
    CloseResultsTab(u64),
    /// The editor for this buffer is the one with keyboard focus this frame.
    FocusedEditor(u64),
}

/// `egui_dock::TabViewer` implementation. Owns short-lived mutable borrows
/// of the underlying app state needed to render each tab variant.
pub struct AppViewer<'a> {
    pub editor: &'a mut EditorState,
    pub results: &'a mut ResultsState,
    pub schema_names: &'a [String],
    pub actions: Vec<DockAction>,
    pub results_actions: Vec<ResultsAction>,
}

impl<'a> AppViewer<'a> {
    pub fn new(
        editor: &'a mut EditorState,
        results: &'a mut ResultsState,
        schema_names: &'a [String],
    ) -> Self {
        Self {
            editor,
            results,
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
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        match tab {
            DockTab::SqlEditor { buffer_id } => egui::Id::new(("dock-tab-editor", *buffer_id)),
            DockTab::Results { tab_id } => egui::Id::new(("dock-tab-results", *tab_id)),
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
        }
        OnCloseResponse::Close
    }
}
