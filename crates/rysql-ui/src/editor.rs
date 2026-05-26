//! SQL editor: buffers, tabs, syntax-highlighted TextEdit, shortcuts and autocomplete.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self,
    text::{CCursor, CCursorRange, LayoutJob},
    Color32, FontId, TextFormat,
};
use rysql_sql::{format_sql, statement_at_cursor, HighlightSpan, Highlighter, SQL_KEYWORDS};

pub struct Buffer {
    pub id: u64,
    pub name: String,
    pub text: String,
    pub dirty: bool,
    /// `Some(p)` — buffer respaldado por un archivo en disco; `name` es el
    /// basename de `p`. `None` — buffer "scratch" todavía no guardado.
    ///
    /// Lo cablea Day 2 (atajos / menú); por ahora sólo lo usan los tests.
    #[allow(dead_code)]
    pub path: Option<PathBuf>,
}

impl Buffer {
    fn new(id: u64, name: String) -> Self {
        Self {
            id,
            name,
            text: String::new(),
            dirty: false,
            path: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AutocompleteState {
    pub prefix: String,
    /// Byte offset where the prefix begins in the buffer.
    pub start_byte: usize,
    pub suggestions: Vec<String>,
    pub selected: usize,
    /// Screen-space anchor for the popup (just below the cursor).
    pub popup_anchor: egui::Pos2,
}

pub struct EditorState {
    pub buffers: Vec<Buffer>,
    /// Index of the buffer that currently has user focus. Updated by
    /// [`render_one`] each time a buffer's TextEdit is interacted with.
    /// Shortcut handlers (`apply_format`, `apply_toggle_comment`,
    /// `resolve_execute`) operate on this buffer.
    pub active: usize,
    pub next_id: u64,
    pub highlighter: Highlighter,
    /// Last known caret byte (primary cursor end), updated each frame.
    pub last_cursor: Option<usize>,
    /// Last known selection (min_byte, max_byte). Equal when no selection.
    pub last_selection: Option<(usize, usize)>,
    pub autocomplete: Option<AutocompleteState>,
}

impl Default for EditorState {
    fn default() -> Self {
        let mut s = Self {
            buffers: Vec::new(),
            active: 0,
            next_id: 0,
            highlighter: Highlighter::new_dark(),
            last_cursor: None,
            last_selection: None,
            autocomplete: None,
        };
        s.new_buffer();
        s
    }
}

impl EditorState {
    /// Create a new buffer named `query-N` and append it to `buffers`.
    /// Returns the stable `buffer_id` of the newly-created buffer.
    pub fn new_buffer(&mut self) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let name = format!("query-{id}");
        self.buffers.push(Buffer::new(id, name));
        self.active = self.buffers.len() - 1;
        self.last_cursor = None;
        self.last_selection = None;
        self.autocomplete = None;
        id
    }

    pub fn close_buffer(&mut self, idx: usize) {
        if idx >= self.buffers.len() {
            return;
        }
        self.buffers.remove(idx);
        if self.active >= self.buffers.len() && !self.buffers.is_empty() {
            self.active = self.buffers.len() - 1;
        }
        self.last_cursor = None;
        self.last_selection = None;
        self.autocomplete = None;
    }

    /// Remove the buffer with the given stable id. No-op if missing.
    pub fn close_buffer_by_id(&mut self, id: u64) {
        if let Some(idx) = self.buffer_index(id) {
            self.close_buffer(idx);
        }
    }

    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.buffers.get(self.active)
    }

    pub fn buffer_by_id(&self, id: u64) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.id == id)
    }

    pub fn buffer_index(&self, id: u64) -> Option<usize> {
        self.buffers.iter().position(|b| b.id == id)
    }

    /// Localiza un buffer cuyo `path` canónico coincida con el argumento.
    /// El caller pasa una ruta ya canonicalizada (es lo que hace
    /// [`Self::open_path`] internamente).
    #[allow(dead_code)] // wired up in Day 2
    pub fn buffer_by_path(&self, path: &Path) -> Option<&Buffer> {
        self.buffers
            .iter()
            .find(|b| b.path.as_deref() == Some(path))
    }

    /// Abre `path` como un buffer respaldado por archivo.
    ///
    /// Si el mismo archivo (comparado por su forma canónica) ya está abierto,
    /// devuelve el id del buffer existente con `loaded = false` y no muta
    /// `buffers`. Si no, lee el contenido (normalizando `\r\n` → `\n`),
    /// añade un nuevo buffer limpio (`dirty = false`) y devuelve su id con
    /// `loaded = true`.
    #[allow(dead_code)] // wired up in Day 2
    pub fn open_path(&mut self, path: PathBuf) -> std::io::Result<(u64, bool)> {
        let canonical = std::fs::canonicalize(&path)?;
        if let Some(existing) = self.buffer_by_path(&canonical) {
            return Ok((existing.id, false));
        }
        let raw = std::fs::read_to_string(&canonical)?;
        let text = raw.replace("\r\n", "\n");

        self.next_id += 1;
        let id = self.next_id;
        let name = canonical
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("query-{id}"));
        self.buffers.push(Buffer {
            id,
            name,
            text,
            dirty: false,
            path: Some(canonical),
        });
        self.active = self.buffers.len() - 1;
        self.last_cursor = None;
        self.last_selection = None;
        self.autocomplete = None;
        Ok((id, true))
    }

    /// Guarda el buffer en su `path` actual. Error si el buffer no tiene
    /// path (usar [`Self::save_as`] primero).
    #[allow(dead_code)] // wired up in Day 2
    pub fn save(&mut self, buffer_id: u64) -> std::io::Result<()> {
        let idx = self
            .buffer_index(buffer_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "buffer not found"))?;
        let buf = &mut self.buffers[idx];
        let path = buf.path.clone().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "buffer has no path; use save_as instead",
            )
        })?;
        std::fs::write(&path, &buf.text)?;
        buf.dirty = false;
        Ok(())
    }

    /// Guarda el buffer a `path`, actualiza su `path` y `name`. Si otro
    /// buffer ya tiene este archivo abierto, devuelve `AlreadyExists` y no
    /// escribe.
    #[allow(dead_code)] // wired up in Day 2
    pub fn save_as(&mut self, buffer_id: u64, path: PathBuf) -> std::io::Result<()> {
        let idx = self
            .buffer_index(buffer_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "buffer not found"))?;

        // Conflict check: sólo posible si el target ya existe en disco
        // (otros buffers almacenan paths canónicos de archivos existentes).
        if path.exists() {
            let canonical = std::fs::canonicalize(&path)?;
            for (i, b) in self.buffers.iter().enumerate() {
                if i == idx {
                    continue;
                }
                if b.path.as_deref() == Some(canonical.as_path()) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "Another tab already has this file open: {}",
                            canonical.display()
                        ),
                    ));
                }
            }
        }

        let text = self.buffers[idx].text.clone();
        std::fs::write(&path, &text)?;
        let canonical = std::fs::canonicalize(&path)?;
        let buf = &mut self.buffers[idx];
        buf.name = canonical
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("query-{}", buf.id));
        buf.path = Some(canonical);
        buf.dirty = false;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum EditorAction {
    NewTab,
    CloseTab,
    Execute(String),
    Format,
    ToggleComment,
}

/// External information the editor needs from the rest of the app.
#[derive(Default)]
pub struct EditorContext<'a> {
    /// Schema-derived names (tables, views, procedures, functions, triggers, events)
    /// from the active connection — used as autocomplete suggestions.
    pub schema_names: &'a [String],
}

pub fn handle_shortcuts(ctx: &egui::Context, focused: bool) -> Vec<EditorAction> {
    if !focused {
        return Vec::new();
    }
    let mut out = Vec::new();
    ctx.input_mut(|i| {
        let enter = egui::Key::Enter;
        let slash = egui::Key::Slash;
        let f = egui::Key::F;
        let t = egui::Key::T;
        let w = egui::Key::W;

        if i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, enter) {
            out.push(EditorAction::Execute(String::new()));
        } else if i.consume_key(egui::Modifiers::COMMAND, enter) {
            out.push(EditorAction::Execute("@cursor".into()));
        }
        // F5 (no modifier) — DataGrip / HeidiSQL convention for "run all".
        // Coexists with Ctrl+Shift+Enter, which keeps its existing role.
        if i.consume_key(egui::Modifiers::NONE, egui::Key::F5) {
            out.push(EditorAction::Execute(String::new()));
        }
        if i.consume_key(egui::Modifiers::COMMAND, slash) {
            out.push(EditorAction::ToggleComment);
        }
        if i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, f) {
            out.push(EditorAction::Format);
        }
        if i.consume_key(egui::Modifiers::COMMAND, t) {
            out.push(EditorAction::NewTab);
        }
        if i.consume_key(egui::Modifiers::COMMAND, w) {
            out.push(EditorAction::CloseTab);
        }
    });
    out
}

#[derive(Debug, Clone, Copy)]
enum AcKey {
    Up,
    Down,
    Accept,
    Dismiss,
}

/// When the autocomplete popup is open, intercept its navigation keys
/// **before** the TextEdit consumes them.
fn consume_autocomplete_keys(ctx: &egui::Context) -> Option<AcKey> {
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            Some(AcKey::Dismiss)
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            Some(AcKey::Down)
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            Some(AcKey::Up)
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            // Accept on Enter when the popup is open. Because we consume
            // the key here it never reaches the `TextEdit`, so no stray
            // newline is inserted. Tab falls back to its native role
            // (focus traversal) and Ctrl+Enter / Ctrl+Shift+Enter still
            // run queries because both carry a modifier.
            Some(AcKey::Accept)
        } else {
            None
        }
    })
}

/// Render a single editor buffer's body (no tab strip — the dock provides
/// tabs). Returns `true` if the underlying TextEdit currently has focus, so
/// the caller can track which buffer is "active" for shortcut routing.
pub fn render_one(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    buffer_id: u64,
    ctx_in: EditorContext<'_>,
) -> bool {
    let Some(buf_idx) = state.buffer_index(buffer_id) else {
        ui.label("Buffer closed");
        return false;
    };
    let egui_ctx = ui.ctx().clone();

    // Stable per-buffer id so the egui TextEditState (cursor, selection)
    // survives moving the tab between leaves.
    let textedit_id = egui::Id::new(("editor-textedit", buffer_id));

    // === Pre-frame: handle autocomplete keys before TextEdit sees them. ===
    // Only intercept if this buffer is the focused one (otherwise an unfocused
    // editor in a split would steal arrow keys).
    let is_active = state.active == buf_idx;
    let ac_key = if is_active && state.autocomplete.is_some() {
        consume_autocomplete_keys(&egui_ctx)
    } else {
        None
    };

    let mut cursor_to_set: Option<usize> = None;
    if let Some(key) = ac_key {
        match key {
            AcKey::Dismiss => state.autocomplete = None,
            AcKey::Down => {
                if let Some(ac) = state.autocomplete.as_mut() {
                    if !ac.suggestions.is_empty() {
                        ac.selected = (ac.selected + 1) % ac.suggestions.len();
                    }
                }
            }
            AcKey::Up => {
                if let Some(ac) = state.autocomplete.as_mut() {
                    if !ac.suggestions.is_empty() {
                        ac.selected =
                            (ac.selected + ac.suggestions.len() - 1) % ac.suggestions.len();
                    }
                }
            }
            AcKey::Accept => {
                if let Some(ac) = state.autocomplete.take() {
                    if let Some(buf) = state.buffers.get_mut(buf_idx) {
                        let suggestion = ac.suggestions[ac.selected].clone();
                        let end = ac.start_byte + ac.prefix.len();
                        if end <= buf.text.len() {
                            buf.text.replace_range(ac.start_byte..end, &suggestion);
                            buf.dirty = true;
                            let new_byte = ac.start_byte + suggestion.len();
                            cursor_to_set = Some(char_index_from_byte(&buf.text, new_byte));
                        }
                    }
                }
            }
        }
    }

    if let Some(new_char_pos) = cursor_to_set {
        if let Some(mut st) = egui::widgets::text_edit::TextEditState::load(&egui_ctx, textedit_id)
        {
            st.cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(new_char_pos))));
            st.store(&egui_ctx, textedit_id);
        }
    }

    let mut new_cursor: Option<usize> = None;
    let mut new_selection: Option<(usize, usize)> = None;
    let mut popup_anchor: Option<egui::Pos2> = None;
    let mut focused = false;

    let Some(buf) = state.buffers.get_mut(buf_idx) else {
        return false;
    };

    let highlighter = &mut state.highlighter;
    let mut layouter = move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = build_layout_job(text.as_str(), highlighter, ui);
        job.wrap.max_width = wrap_width;
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };

    let before = buf.text.clone();
    // 8 px padding on every side so the cursor doesn't kiss the dock
    // border. Wrapped OUTSIDE the ScrollArea so the padding stays
    // visible while content scrolls.
    egui::Frame::default()
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    let output = egui::TextEdit::multiline(&mut buf.text)
                        .id(textedit_id)
                        .font(egui::TextStyle::Monospace)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(20)
                        .layouter(&mut layouter)
                        .show(ui);

                    if buf.text != before {
                        buf.dirty = true;
                    }
                    focused = output.response.has_focus();
                    if let Some(range) = output.cursor_range {
                        let primary_byte = byte_index_from_char(&buf.text, range.primary.index);
                        let secondary_byte = byte_index_from_char(&buf.text, range.secondary.index);
                        new_cursor = Some(primary_byte);
                        new_selection = Some((
                            primary_byte.min(secondary_byte),
                            primary_byte.max(secondary_byte),
                        ));

                        // Popup anchor: just below the cursor in screen space.
                        let rect = output.galley.pos_from_cursor(range.primary);
                        popup_anchor = Some(egui::Pos2 {
                            x: output.galley_pos.x + rect.min.x,
                            y: output.galley_pos.y + rect.max.y + 2.0,
                        });
                    }
                });
        });

    // Keep cursor/selection/autocomplete tied to the focused buffer so
    // shortcuts and the popup target the right one.
    if focused {
        state.active = buf_idx;
        state.last_cursor = new_cursor;
        state.last_selection = new_selection;
        refresh_autocomplete(state, ctx_in.schema_names, popup_anchor);
        if let Some(ac) = state.autocomplete.as_ref() {
            render_autocomplete_popup(&egui_ctx, ac);
        }
    } else if is_active {
        // Was the focused buffer last frame, lost focus this frame: drop the
        // popup so it doesn't linger over an inactive editor.
        state.autocomplete = None;
    }

    focused
}

fn refresh_autocomplete(
    state: &mut EditorState,
    schema_names: &[String],
    anchor: Option<egui::Pos2>,
) {
    let Some(cursor) = state.last_cursor else {
        state.autocomplete = None;
        return;
    };
    let Some((sel_min, sel_max)) = state.last_selection else {
        state.autocomplete = None;
        return;
    };
    if sel_min != sel_max {
        state.autocomplete = None;
        return;
    }
    let Some(buf) = state.buffers.get(state.active) else {
        state.autocomplete = None;
        return;
    };

    let (start, prefix) = current_word(&buf.text, cursor);
    if prefix.len() < 2 {
        state.autocomplete = None;
        return;
    }

    let suggestions = compute_suggestions(&prefix, schema_names);
    if suggestions.is_empty() {
        state.autocomplete = None;
        return;
    }

    let prev_selected = state
        .autocomplete
        .as_ref()
        .filter(|p| p.prefix == prefix)
        .map(|p| p.selected)
        .unwrap_or(0)
        .min(suggestions.len() - 1);

    let popup_anchor = anchor.unwrap_or(egui::Pos2::ZERO);

    state.autocomplete = Some(AutocompleteState {
        prefix,
        start_byte: start,
        suggestions,
        selected: prev_selected,
        popup_anchor,
    });
}

fn render_autocomplete_popup(ctx: &egui::Context, ac: &AutocompleteState) {
    egui::Area::new(egui::Id::new("editor-autocomplete-popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(ac.popup_anchor)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.set_min_width(220.0);
                    for (i, s) in ac.suggestions.iter().enumerate() {
                        let is_sel = i == ac.selected;
                        let text = egui::RichText::new(s).monospace();
                        let text = if is_sel {
                            text.background_color(ui.visuals().selection.bg_fill)
                                .color(ui.visuals().selection.stroke.color)
                        } else {
                            text
                        };
                        ui.label(text);
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Enter to accept · Esc to dismiss · ↑↓ to navigate")
                            .weak()
                            .small(),
                    );
                });
        });
}

fn compute_suggestions(prefix: &str, schema_names: &[String]) -> Vec<String> {
    const MAX: usize = 12;
    let lower = prefix.to_ascii_lowercase();
    let mut schema_matches: Vec<String> = schema_names
        .iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&lower))
        .filter(|n| n.as_str() != prefix)
        .cloned()
        .collect();
    schema_matches.sort();
    schema_matches.dedup();

    let mut keyword_matches: Vec<String> = SQL_KEYWORDS
        .iter()
        .filter(|k| k.to_ascii_lowercase().starts_with(&lower))
        .filter(|k| **k != prefix.to_ascii_uppercase())
        .map(|k| (*k).to_string())
        .collect();
    keyword_matches.sort();

    let mut out = Vec::with_capacity(MAX);
    for n in schema_matches.into_iter().chain(keyword_matches) {
        if out.len() == MAX {
            break;
        }
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// Identifier characters: `[A-Za-z0-9_]`. Returns (start_byte, prefix).
fn current_word(text: &str, cursor: usize) -> (usize, String) {
    let bytes = text.as_bytes();
    let mut start = cursor.min(bytes.len());
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    let prefix = text[start..cursor.min(text.len())].to_string();
    (start, prefix)
}

fn byte_index_from_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn char_index_from_byte(text: &str, byte_idx: usize) -> usize {
    if byte_idx >= text.len() {
        return text.chars().count();
    }
    text[..byte_idx].chars().count()
}

pub(crate) fn build_layout_job(
    text: &str,
    highlighter: &mut Highlighter,
    ui: &egui::Ui,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font_id = FontId::monospace(ui.style().text_styles[&egui::TextStyle::Monospace].size);
    for line_with_nl in split_keep_newlines(text) {
        let spans: Vec<HighlightSpan> = highlighter.highlight_line(line_with_nl);
        for span in spans {
            let fmt = TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(span.color[0], span.color[1], span.color[2]),
                ..Default::default()
            };
            job.append(&span.text, 0.0, fmt);
        }
    }
    job
}

fn split_keep_newlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            out.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Apply [`EditorAction::Format`] to the active buffer in place.
pub fn apply_format(state: &mut EditorState) {
    if let Some(buf) = state.buffers.get_mut(state.active) {
        let new = format_sql(&buf.text);
        if new != buf.text {
            buf.text = new;
            buf.dirty = true;
        }
    }
}

/// Apply [`EditorAction::ToggleComment`]. If a selection is present, operate on
/// every line that the selection touches; otherwise on the line containing the
/// cursor; otherwise on the entire buffer (when no cursor info is available).
pub fn apply_toggle_comment(state: &mut EditorState) {
    let cursor = state.last_cursor;
    let selection = state.last_selection;
    let Some(buf) = state.buffers.get_mut(state.active) else {
        return;
    };

    let (block_start, block_end) = match (selection, cursor) {
        (Some((a, b)), _) if a != b => expand_to_lines(&buf.text, a, b),
        (_, Some(c)) => expand_to_lines(&buf.text, c, c),
        _ => (0, buf.text.len()),
    };
    if block_end <= block_start {
        return;
    }

    let block = buf.text[block_start..block_end].to_string();
    let lines: Vec<&str> = block.split_inclusive('\n').collect();
    let all_commented = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with("-- "));

    let mut out = String::with_capacity(block.len() + lines.len() * 3);
    for line in lines {
        if line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        if all_commented {
            if let Some(idx) = line.find("-- ") {
                out.push_str(&line[..idx]);
                out.push_str(&line[idx + 3..]);
            } else {
                out.push_str(line);
            }
        } else {
            let leading_ws_end = line
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(0);
            out.push_str(&line[..leading_ws_end]);
            out.push_str("-- ");
            out.push_str(&line[leading_ws_end..]);
        }
    }
    if out != block {
        buf.text.replace_range(block_start..block_end, &out);
        buf.dirty = true;
    }
}

fn expand_to_lines(text: &str, lo: usize, hi: usize) -> (usize, usize) {
    let lo = lo.min(text.len());
    let hi = hi.min(text.len());
    let start = text[..lo].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = if hi == text.len() {
        text.len()
    } else {
        text[hi..]
            .find('\n')
            .map(|i| hi + i + 1)
            .unwrap_or(text.len())
    };
    (start, end)
}

/// Resolve the SQL string to execute for an [`EditorAction::Execute`] arg.
pub fn resolve_execute(
    state: &EditorState,
    arg: &str,
    cursor_byte: Option<usize>,
) -> Option<String> {
    let buf = state.active_buffer()?;
    if arg == "@cursor" {
        let cur = cursor_byte.or(state.last_cursor).unwrap_or(buf.text.len());
        let range = statement_at_cursor(&buf.text, cur)?;
        let s = buf.text[range].trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        let s = buf.text.trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Genera una ruta única dentro de `temp_dir` para aislar tests
    /// concurrentes sin necesitar `tempfile` como dep.
    fn temp_path() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rysql-editor-test-{}-{}.sql",
            std::process::id(),
            n,
        ))
    }

    fn write_temp_sql(content: &str) -> PathBuf {
        let path = temp_path();
        std::fs::write(&path, content).expect("write temp file");
        path
    }

    #[test]
    fn open_path_loads_file_clean() {
        let path = write_temp_sql("SELECT 1;");
        let mut state = EditorState::default();
        let (id, loaded) = state.open_path(path.clone()).expect("open");
        assert!(loaded, "first open should report loaded = true");
        let buf = state.buffer_by_id(id).expect("buffer present");
        assert_eq!(buf.text, "SELECT 1;");
        assert!(!buf.dirty);
        assert_eq!(buf.path.as_deref().map(|p| p.exists()), Some(true));
        let basename = path.file_name().unwrap().to_string_lossy();
        assert_eq!(buf.name, basename);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_path_normalizes_crlf() {
        let path = write_temp_sql("SELECT 1;\r\nSELECT 2;\r\n");
        let mut state = EditorState::default();
        let (id, _) = state.open_path(path.clone()).expect("open");
        let buf = state.buffer_by_id(id).expect("buffer present");
        assert_eq!(buf.text, "SELECT 1;\nSELECT 2;\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_path_missing_file_errors_and_does_not_mutate() {
        let path = temp_path();
        let mut state = EditorState::default();
        let before = state.buffers.len();
        let err = state
            .open_path(path)
            .expect_err("missing file should error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(state.buffers.len(), before);
    }

    #[test]
    fn open_path_deduplicates_same_file() {
        let path = write_temp_sql("SELECT 1;");
        let mut state = EditorState::default();
        let (id1, loaded1) = state.open_path(path.clone()).expect("open 1");
        let len_after_first = state.buffers.len();
        let (id2, loaded2) = state.open_path(path.clone()).expect("open 2");
        assert!(loaded1);
        assert!(!loaded2, "second open of same file should not load");
        assert_eq!(id1, id2);
        assert_eq!(state.buffers.len(), len_after_first);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_writes_disk_and_clears_dirty() {
        let path = write_temp_sql("SELECT 1;");
        let mut state = EditorState::default();
        let (id, _) = state.open_path(path.clone()).expect("open");
        let idx = state.buffer_index(id).expect("idx");
        state.buffers[idx].text = "SELECT 99;".into();
        state.buffers[idx].dirty = true;

        state.save(id).expect("save");
        let buf = state.buffer_by_id(id).expect("buffer present");
        assert!(!buf.dirty);
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, "SELECT 99;");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_without_path_errors() {
        let mut state = EditorState::default();
        let id = state.buffers[0].id;
        state.buffers[0].text = "SELECT 1;".into();
        state.buffers[0].dirty = true;
        let err = state.save(id).expect_err("scratch buffer cannot save");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn save_as_assigns_path_and_writes() {
        let mut state = EditorState::default();
        let id = state.buffers[0].id;
        state.buffers[0].text = "SELECT 1;".into();
        state.buffers[0].dirty = true;

        let target = temp_path();
        state.save_as(id, target.clone()).expect("save_as");
        let buf = state.buffer_by_id(id).expect("buffer present");
        assert!(!buf.dirty);
        let stored = buf.path.clone().expect("path set");
        // `path` queda canonicalizado.
        assert!(stored.exists());
        let on_disk = std::fs::read_to_string(&target).expect("read");
        assert_eq!(on_disk, "SELECT 1;");
        let expected_name = target.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(buf.name, expected_name);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn save_as_rejects_path_owned_by_another_buffer() {
        let path = write_temp_sql("SELECT 1;");
        let mut state = EditorState::default();
        let (file_id, _) = state.open_path(path.clone()).expect("open");
        let scratch_id = state.new_buffer();
        state
            .buffers
            .iter_mut()
            .find(|b| b.id == scratch_id)
            .unwrap()
            .text = "SELECT 2;".into();
        let err = state
            .save_as(scratch_id, path.clone())
            .expect_err("conflict expected");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // El primer buffer no se ha tocado.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, "SELECT 1;");
        // Y el file_id sigue limpio.
        assert!(!state.buffer_by_id(file_id).unwrap().dirty);
        let _ = std::fs::remove_file(&path);
    }
}
