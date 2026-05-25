use eframe::egui;

pub struct RysqlApp {}

impl RysqlApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {}
    }
}

impl eframe::App for RysqlApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        egui::Panel::top("menu_bar").show_inside(ui, |ui| {
            egui::containers::menu::MenuBar::new().ui(ui, |ui| {
                egui::containers::menu::MenuButton::new("File").ui(ui, |ui| {
                    if ui.button("New connection…").clicked() {
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                egui::containers::menu::MenuButton::new("Edit").ui(ui, |_ui| {});
                egui::containers::menu::MenuButton::new("View").ui(ui, |_ui| {});
                egui::containers::menu::MenuButton::new("Help").ui(ui, |ui| {
                    if ui.button("About RySQL").clicked() {
                        ui.close();
                    }
                });
            });
        });

        egui::Panel::bottom("status_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Disconnected");
                ui.separator();
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading("RySQL");
                ui.label("MySQL / MariaDB client — scaffold");
            });
        });
    }
}
