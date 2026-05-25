//! `egui_dock` glue: the [`DockTab`] enum is the discriminator for every tab
//! that lives in the central dock area; [`AppViewer`] is a thin
//! [`egui_dock::TabViewer`] adapter that delegates rendering to the existing
//! per-module renderers (currently just [`crate::editor`]) and collects
//! [`DockAction`] intents that the caller applies after the dock has been
//! rendered.
//!
//! Day 1 scope: only `SqlEditor` tabs. Future days add `Results` and
//! `Object` variants per `docs/dock-layout-plan.md`.

use eframe::egui;
use egui_dock::{tab_viewer::OnCloseResponse, TabViewer};

use crate::editor::{self, EditorContext, EditorState};

/// Discriminator for a tab living in the central [`egui_dock::DockState`].
///
/// The tab itself carries only an id; the actual state lives in `RysqlApp`
/// (e.g. [`EditorState::buffers`] indexed by [`crate::editor::Buffer::id`]),
/// keeping the dock state small and easy to serialize later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DockTab {
    /// SQL editor tab backed by the buffer with this stable id.
    SqlEditor { buffer_id: u64 },
}

/// Intent collected during a dock render frame, applied by `RysqlApp` once
/// the borrow on app state held by [`AppViewer`] has been released.
#[derive(Debug, Clone)]
pub enum DockAction {
    /// The user clicked the close button on an editor tab; free the buffer.
    CloseEditorBuffer(u64),
    /// The editor for this buffer is the one with keyboard focus this frame.
    FocusedEditor(u64),
}

/// `egui_dock::TabViewer` implementation. Owns short-lived mutable borrows
/// of the underlying app state needed to render each tab variant.
pub struct AppViewer<'a> {
    pub editor: &'a mut EditorState,
    pub schema_names: &'a [String],
    pub actions: Vec<DockAction>,
}

impl<'a> AppViewer<'a> {
    pub fn new(editor: &'a mut EditorState, schema_names: &'a [String]) -> Self {
        Self {
            editor,
            schema_names,
            actions: Vec::new(),
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
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        match tab {
            DockTab::SqlEditor { buffer_id } => egui::Id::new(("dock-tab-editor", *buffer_id)),
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
        }
    }

    fn on_close(&mut self, tab: &mut Self::Tab) -> OnCloseResponse {
        match tab {
            DockTab::SqlEditor { buffer_id } => {
                self.actions.push(DockAction::CloseEditorBuffer(*buffer_id));
            }
        }
        OnCloseResponse::Close
    }
}
