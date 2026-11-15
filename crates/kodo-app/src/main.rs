//! kodo desktop app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod table;
mod theme;
mod view;

use crate::app::App;

fn main() -> iced::Result {
    // Window settings need the saved size/position before boot runs.
    let portable = std::env::args().any(|a| a == "--portable");
    let config = kodo_core::config::Paths::resolve(portable)
        .ok()
        .and_then(|p| kodo_core::config::Config::load(&p.config_file).ok())
        .unwrap_or_default();
    iced::application(App::boot, App::update, view::view)
        .title("kodo")
        .theme(App::theme)
        .scale_factor(App::scale)
        .subscription(App::subscription)
        .window(App::window_settings(&config))
        .font(theme::INTER_REGULAR)
        .font(theme::INTER_SEMIBOLD)
        .font(theme::LUCIDE)
        .default_font(theme::FONT)
        .run()
}
