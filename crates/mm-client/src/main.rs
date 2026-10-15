mod app;
mod chat;
mod chat_view;
mod session;
mod store;
mod style;
mod switcher;
mod text;
mod viewer;

use app::App;

fn main() -> iced::Result {
    style::set_font(omarchy_theme::monospace_font());
    let mut window = iced::window::Settings { size: iced::Size::new(1200.0, 800.0), ..Default::default() };
    // Wayland app id / X11 class, so Hyprland rules and .desktop files can match it.
    window.platform_specific.application_id = "mm-rust".into();
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window)
        .default_font(style::font())
        .run()
}
