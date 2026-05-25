//! Syntax-highlighting via syntect, framework-agnostic.

use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// A contiguous run of text that should be drawn in a single colour.
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub text: String,
    pub color: [u8; 3],
}

pub struct Highlighter {
    syntax_set: SyntaxSet,
    syntax: SyntaxReference,
    theme: Theme,
}

impl Highlighter {
    /// Highlighter tuned for dark UIs.
    pub fn new_dark() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let syntax = syntax_set
            .find_syntax_by_extension("sql")
            .or_else(|| syntax_set.find_syntax_by_name("SQL"))
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text())
            .clone();
        let theme = theme_set
            .themes
            .get("base16-mocha.dark")
            .or_else(|| theme_set.themes.get("base16-ocean.dark"))
            .cloned()
            .unwrap_or_else(|| theme_set.themes.values().next().unwrap().clone());
        Self {
            syntax_set,
            syntax,
            theme,
        }
    }

    /// Highlight a single line (must include its trailing newline if any).
    /// Returns one span per coloured run; never empty for non-empty input.
    pub fn highlight_line(&mut self, line: &str) -> Vec<HighlightSpan> {
        let mut h = HighlightLines::new(&self.syntax, &self.theme);
        match h.highlight_line(line, &self.syntax_set) {
            Ok(regions) => regions
                .into_iter()
                .map(|(style, text)| HighlightSpan {
                    text: text.to_string(),
                    color: style_color(style),
                })
                .collect(),
            Err(_) => vec![HighlightSpan {
                text: line.to_string(),
                color: [220, 220, 220],
            }],
        }
    }
}

fn style_color(s: Style) -> [u8; 3] {
    [s.foreground.r, s.foreground.g, s.foreground.b]
}
