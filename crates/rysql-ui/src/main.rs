use eframe::egui;

mod app;
mod bridge;
mod dialog;
mod editor;
mod fonts;
mod history_view;
mod logging;
mod results;
mod runtime;
mod sidebar;
mod state;

use app::RysqlApp;

fn main() -> eframe::Result<()> {
    let _log_guard = logging::init();

    // Pre-warm the tokio runtime so it's ready when the first frame renders.
    let _ = runtime::handle();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RySQL")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RySQL",
        native_options,
        Box::new(|cc| Ok(Box::new(RysqlApp::new(cc)))),
    )
}
