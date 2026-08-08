use eframe::egui;

mod app;
mod bridge;
mod column_dialog;
mod dialog;
mod dock;
mod editor;
mod export_dialog;
mod fonts;
mod history_view;
mod logging;
mod object_view;
mod results;
mod runtime;
mod sidebar;
mod state;

use app::RysqlApp;

fn main() -> eframe::Result<()> {
    let _log_guard = logging::init();

    // Pre-warm the tokio runtime so it's ready when the first frame renders.
    let _ = runtime::handle();

    // `app_id` becomes the Wayland xdg-shell app_id / Hyprland window class.
    // Must match StartupWMClass in packaging/linux/rysql.desktop so the
    // compositor can identify the window (avoids "RySQL - (desconocido)"
    // in ANR dialogs) and associate icons / desktop actions.
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RySQL")
            .with_app_id("RySQL")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        // On Linux (esp. Hyprland/Wayland), vsync Wait can block the UI
        // thread in eglSwapBuffers when the window is on an inactive
        // workspace and the compositor stops sending frame callbacks.
        // That freezes the event loop long enough for Hyprland's ANR
        // dialog ("La aplicación no responde"). DontWait keeps pings
        // answered; minor tearing is preferable to false freezes.
        #[cfg(target_os = "linux")]
        vsync: false,
        ..Default::default()
    };

    eframe::run_native(
        "RySQL",
        native_options,
        Box::new(|cc| Ok(Box::new(RysqlApp::new(cc)))),
    )
}
