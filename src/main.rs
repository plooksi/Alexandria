#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fsx;
mod pane;
mod theme;

fn main() -> eframe::Result<()> {
    //App name
    let name = &format!("Alexandria v{}", env!("CARGO_PKG_VERSION"));
    //App icon streaming
    let img = image::load_from_memory_with_format(
    include_bytes!("../assets/icon.ico"),
    image::ImageFormat::Ico,
    )
    .expect("failed to load icon")
    .into_rgba8();

let (width, height) = img.dimensions();
let icon_data = eframe::egui::IconData {
    rgba: img.into_raw(),
    width,
    height,
};

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(name)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([820.0, 480.0])
            .with_icon(icon_data)
            .with_decorations(false) // custom title bar + caption buttons
            .with_transparent(true), // required for the acrylic effect
        ..Default::default()
    };
    eframe::run_native(
        name,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
