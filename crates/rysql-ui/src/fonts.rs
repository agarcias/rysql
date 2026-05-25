//! Custom font setup: Inter for proportional text, JetBrains Mono for code.

use std::sync::Arc;

use eframe::egui::{FontData, FontDefinitions, FontFamily};

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

pub fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();

    fonts
        .font_data
        .insert("Inter".into(), Arc::new(FontData::from_static(INTER)));
    fonts.font_data.insert(
        "JetBrainsMono".into(),
        Arc::new(FontData::from_static(JETBRAINS_MONO)),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono".into());

    fonts
}
