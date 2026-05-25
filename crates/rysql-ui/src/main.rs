use eframe::egui;
use tracing_subscriber::EnvFilter;

mod app;
mod bridge;
mod dialog;
mod editor;
mod fonts;
mod results;
mod runtime;
mod sidebar;
mod state;

use app::RysqlApp;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

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
