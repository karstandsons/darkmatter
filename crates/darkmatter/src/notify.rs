//! Desktop notifications, and clicks on them: a click opens the message, on
//! its own server (docs/darkmatter/MULTI_SERVER.md, Notifications).

use std::sync::{Mutex, OnceLock};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Where a click on a notification goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click {
    /// The account's id (`accounts.toml`).
    pub account: String,
    pub channel: String,
    /// The thread's root, or the post itself.
    pub root: String,
    pub post: String,
}

type Clicks = (UnboundedSender<Click>, Mutex<Option<UnboundedReceiver<Click>>>);

fn clicks_channel() -> &'static Clicks {
    static CLICKS: OnceLock<Clicks> = OnceLock::new();
    CLICKS.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        (tx, Mutex::new(Some(rx)))
    })
}

/// What a message's notification says.
pub struct Note {
    pub title: String,
    pub body: String,
}

/// The notification for a message: from whom and where, and on which server
/// once there's more than one. In an encrypted conversation the text stays
/// out of the desktop's notification history unless `previews` is on.
pub fn note(server: Option<&str>, sender: &str, channel: &str, message: &str, encrypted: bool, previews: bool) -> Note {
    let title = match server {
        Some(server) => format!("{server}: {sender} in {channel}"),
        None => format!("{sender} in {channel}"),
    };
    let body = if encrypted && !previews { "🔒 New message".to_string() } else { message.to_string() };
    Note { title, body }
}

/// Shows a notification; a click on it comes back through [`clicks`].
pub fn show(note: Note, click: Click) {
    // D-Bus / macOS notification calls block briefly; keep them off the UI
    // thread. On Linux the thread then waits for a click.
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Darkmatter").summary(&note.title).body(&note.body);
        #[cfg(all(unix, not(target_os = "macos")))]
        n.action("default", "Open");
        match n.show() {
            #[cfg(all(unix, not(target_os = "macos")))]
            Ok(handle) => handle.wait_for_action(|action| {
                if action == "default" {
                    let _ = clicks_channel().0.send(click);
                }
            }),
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            Ok(_) => drop(click),
            Err(e) => tracing::warn!("notification failed: {e}"),
        }
    });
}

/// Clicks on notifications, for the app's subscription (it runs once).
pub fn clicks() -> impl iced::futures::Stream<Item = Click> {
    let rx = clicks_channel().1.lock().ok().and_then(|mut rx| rx.take());
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let click = rx.recv().await?;
        Some((click, Some(rx)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_name_the_server_and_keep_encrypted_text_out() {
        let n = note(Some("Family"), "bob", "Town Square", "the secret", true, false);
        assert_eq!((n.title.as_str(), n.body.as_str()), ("Family: bob in Town Square", "🔒 New message"));
        let n = note(None, "bob", "dev", "plain", false, false);
        assert_eq!((n.title.as_str(), n.body.as_str()), ("bob in dev", "plain"));
        assert_eq!(note(None, "bob", "dev", "the secret", true, true).body, "the secret", "previews on");
    }
}
