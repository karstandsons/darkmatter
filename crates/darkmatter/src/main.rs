mod account;
mod animated;
mod app;
mod chat;
mod chat_view;
mod commands;
mod config;
mod crypto;
mod emoji_picker;
mod layout;
mod lightbox;
mod media;
mod outbox;
mod pane;
mod panels;
mod plugin_picker;
mod plugin_manager;
mod plugins;
mod report;
mod session;
mod shortcuts;
mod store;
mod style;
mod switcher;
mod text;
mod theme_picker;
mod update;
mod vault;
mod viewer;
mod zoom;

use app::App;

/// Darkmatter used to be called mm-rust: move its folders (settings, saved
/// layout, cache) to the new name once. The keyring login moves in `session`.
fn migrate_old_name() {
    let bases = [dirs::config_dir(), dirs::state_dir().or_else(dirs::data_local_dir), dirs::cache_dir()];
    for base in bases.into_iter().flatten() {
        let (old, new) = (base.join("mm-rust"), base.join("darkmatter"));
        if old.is_dir() && !new.exists() {
            match std::fs::rename(&old, &new) {
                Ok(()) => tracing::info!("moved {} to {}", old.display(), new.display()),
                Err(e) => tracing::warn!("couldn't move {}: {e}", old.display()),
            }
        }
    }
}

fn main() -> iced::Result {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("darkmatter {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "plugins" || a == "plugin") {
        std::process::exit(plugins::cli(&args[1..]));
    }
    // RUST_LOG=darkmatter=debug logs every UI message (except the noisy ones).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();
    migrate_old_name();
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
        window.platform_specific.application_id = "darkmatter".into();
    }
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window)
        .default_font(style::font())
        .run()
}
