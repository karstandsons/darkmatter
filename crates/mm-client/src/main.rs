mod app;
mod chat;
mod chat_view;
mod config;
mod emoji_picker;
mod media;
mod pane;
mod panels;
mod session;
mod store;
mod style;
mod switcher;
mod text;
mod theme_picker;
mod viewer;

use app::App;

fn main() -> iced::Result {
    // RUST_LOG=mm_rust=debug logs every UI message (except the noisy ones).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();
    // Read-only: never change anything on the server (for trying it on a real one).
    let read_only = std::env::args().any(|a| a == "--read-only")
        || std::env::var("MM_READONLY").is_ok_and(|v| !v.is_empty() && v != "0");
    session::set_read_only(read_only);
    if read_only {
        tracing::info!("read-only mode: writes to the server are refused");
    }
    style::set_font(config::Config::load().theme.font.or_else(omarchy_theme::monospace_font));
    #[allow(unused_mut)]
    let mut window = iced::window::Settings { size: iced::Size::new(1200.0, 800.0), ..Default::default() };
    // Wayland app id / X11 class, so Hyprland rules and .desktop files can match it.
    #[cfg(target_os = "linux")]
    {
        window.platform_specific.application_id = "mm-rust".into();
    }
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window)
        .default_font(style::font())
        .run()
}
