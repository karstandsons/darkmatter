//! Every command Darkmatter has, in one list: its title, keys, slash name and
//! group. Key handling, slash commands, the ☰ menu, Ctrl+K and the shortcuts
//! sheet all read from here, so they never disagree.

use iced::keyboard::{self, Key, key::Named};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    QuickSwitcher,
    Search,
    Mentions,
    Inbox,
    Threads,
    PrevChannel,
    NextChannel,
    /// Servers, on the rail.
    PrevServer,
    NextServer,
    MentionServer,
    Server,
    AddServer,
    /// The shared inbox: DMs and mentions from every server.
    AllInbox,
    SplitRight,
    SplitDown,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ToggleSidebar,
    ToggleDensity,
    Compact,
    Cozy,
    ThreadLayout,
    Attach,
    ReactLatest,
    ReplyLatest,
    Leave,
    CreateChannel,
    Themes,
    Plugins,
    Account,
    Shortcuts,
    About,
    Logout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Go,
    View,
    Messages,
    Darkmatter,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Go, Group::View, Group::Messages, Group::Darkmatter];

    pub fn title(self) -> &'static str {
        match self {
            Group::Go => "Go",
            Group::View => "View",
            Group::Messages => "Messages",
            Group::Darkmatter => "Darkmatter",
        }
    }
}

/// A key with modifiers. `key` is a character ("k", "\\") or a named key
/// ("ArrowUp"). `cmd` is Ctrl, or ⌘ on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key: &'static str,
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
}

const fn ctrl(key: &'static str) -> Chord {
    Chord { key, cmd: true, shift: false, alt: false }
}
const fn ctrl_shift(key: &'static str) -> Chord {
    Chord { key, cmd: true, shift: true, alt: false }
}
const fn ctrl_alt(key: &'static str) -> Chord {
    Chord { key, cmd: true, shift: false, alt: true }
}
const fn alt(key: &'static str) -> Chord {
    Chord { key, cmd: false, shift: false, alt: true }
}
const fn shift(key: &'static str) -> Chord {
    Chord { key, cmd: false, shift: true, alt: false }
}

pub struct Spec {
    pub command: Command,
    pub title: &'static str,
    pub group: Group,
    pub keys: &'static [Chord],
    /// Slash names; the first is the one shown. Several words ("split down") are fine.
    pub slash: &'static [&'static str],
    /// What follows the slash name, for the hint ("[terms]").
    pub args: &'static str,
}

const fn spec(
    command: Command,
    title: &'static str,
    group: Group,
    keys: &'static [Chord],
    slash: &'static [&'static str],
    args: &'static str,
) -> Spec {
    Spec { command, title, group, keys, slash, args }
}

use Command as C;
pub const ALL: &[Spec] = &[
    spec(C::QuickSwitcher, "Jump to a channel or person", Group::Go, &[ctrl("k")], &["jump"], "[name]"),
    spec(C::Search, "Search messages", Group::Go, &[ctrl("f")], &["search"], "[terms]"),
    spec(C::Mentions, "Recent mentions", Group::Go, &[ctrl_shift("m")], &["mentions"], ""),
    spec(C::Inbox, "Unread inbox", Group::Go, &[ctrl("i")], &["inbox"], ""),
    spec(C::Threads, "Followed threads", Group::Go, &[], &["threads"], ""),
    spec(C::PrevChannel, "Previous channel", Group::Go, &[alt("ArrowUp")], &[], ""),
    spec(C::NextChannel, "Next channel", Group::Go, &[alt("ArrowDown")], &[], ""),
    spec(C::PrevServer, "Previous server", Group::Go, &[ctrl_shift("Tab")], &[], ""),
    spec(C::NextServer, "Next server", Group::Go, &[ctrl("Tab")], &[], ""),
    spec(C::MentionServer, "Next server with mentions", Group::Go, &[ctrl_alt("a")], &[], ""),
    spec(C::Server, "Go to a server", Group::Go, &[], &["server"], "[name]"),
    spec(C::AddServer, "Add a server", Group::Go, &[], &["server add"], ""),
    spec(C::AllInbox, "DMs and mentions from every server", Group::Go, &[ctrl_shift("i")], &["all"], ""),
    spec(C::SplitRight, "Split right", Group::View, &[ctrl("d")], &["split", "split right"], ""),
    spec(C::SplitDown, "Split down", Group::View, &[ctrl_shift("d")], &["split down"], ""),
    spec(C::ClosePane, "Close pane", Group::View, &[ctrl("w")], &["close"], ""),
    spec(C::FocusLeft, "Pane to the left", Group::View, &[ctrl_alt("ArrowLeft")], &[], ""),
    spec(C::FocusRight, "Pane to the right", Group::View, &[ctrl_alt("ArrowRight")], &[], ""),
    spec(C::FocusUp, "Pane above", Group::View, &[ctrl_alt("ArrowUp")], &[], ""),
    spec(C::FocusDown, "Pane below", Group::View, &[ctrl_alt("ArrowDown")], &[], ""),
    spec(C::ToggleSidebar, "Show / hide the sidebar", Group::View, &[ctrl("b")], &["sidebar"], ""),
    spec(C::ToggleDensity, "Compact / cozy", Group::View, &[ctrl_shift("l")], &["density"], ""),
    spec(C::Compact, "Compact", Group::View, &[], &["compact"], ""),
    spec(C::Cozy, "Cozy", Group::View, &[], &["cozy"], ""),
    spec(C::ThreadLayout, "Threads inline / in a side pane", Group::View, &[ctrl_shift("t")], &["threadview"], ""),
    spec(C::Attach, "Attach files", Group::Messages, &[ctrl("u")], &["upload"], ""),
    spec(C::ReactLatest, "React to the latest message", Group::Messages, &[ctrl_shift("\\")], &[], ""),
    spec(C::ReplyLatest, "Reply to the latest message", Group::Messages, &[shift("ArrowUp")], &[], ""),
    spec(C::Leave, "Leave this channel", Group::Messages, &[], &[], ""),
    spec(C::CreateChannel, "New channel (private ones are encrypted on axiond)", Group::Messages, &[], &["create"], "[private] <name>"),
    spec(C::Themes, "Themes", Group::Darkmatter, &[ctrl("t")], &["theme"], "[name]"),
    spec(C::Plugins, "Plugins", Group::Darkmatter, &[ctrl_shift("p")], &["plugins"], ""),
    spec(C::Account, "Account and security", Group::Darkmatter, &[], &["account"], ""),
    spec(C::Shortcuts, "Keyboard shortcuts", Group::Darkmatter, &[ctrl("/")], &["shortcuts", "keys"], ""),
    spec(C::About, "About Darkmatter", Group::Darkmatter, &[], &["about"], ""),
    spec(C::Logout, "Log out", Group::Darkmatter, &[], &["logout"], ""),
];

/// Keys that aren't commands of their own, for the shortcuts sheet.
pub const NOTES: &[(Group, &str, &str)] = &[
    (Group::Messages, "↑ in an empty box", "Edit your last message"),
    (Group::Messages, "Enter / Shift+Enter", "Send / new line"),
    (Group::Messages, "/ at the start", "Slash commands (Tab completes)"),
    (Group::Go, "PageUp / PageDown", "Scroll the messages"),
    (Group::Go, SERVER_KEYS, "The 1st…9th server on the rail"),
    (Group::Go, "Esc", "Close what's open, then select messages"),
];

/// Vim-style keys, when no text box has the cursor (`[keys] vim`).
pub const VIM: &[(&str, &str)] = &[
    ("j / k", "Next / previous message"),
    ("gg / G", "Oldest / newest"),
    ("h / l", "Pane left / right"),
    ("r", "Reply"),
    ("o", "Show the replies"),
    ("e", "Edit"),
    ("dd", "Delete"),
    ("+", "React"),
    ("u", "Mark unread from here"),
    ("i", "Back to the message box"),
    ("/", "Search"),
    ("?", "Keyboard shortcuts"),
];

pub fn spec_of(command: Command) -> &'static Spec {
    ALL.iter().find(|s| s.command == command).expect("every command has a spec")
}

impl Chord {
    fn matches(&self, key: &Key, modifiers: keyboard::Modifiers) -> bool {
        if modifiers.command() != self.cmd || modifiers.shift() != self.shift || modifiers.alt() != self.alt {
            return false;
        }
        match key {
            // Shift changes the character on some keys ("\\" → "|").
            Key::Character(k) => k.eq_ignore_ascii_case(self.key) || (self.key == "\\" && k.as_str() == "|"),
            Key::Named(named) => named_str(*named) == Some(self.key),
            _ => false,
        }
    }

    /// "Ctrl+Shift+D", or "⌘⇧D" on macOS.
    pub fn label(&self) -> String {
        let key = match self.key {
            "ArrowUp" => "↑".to_string(),
            "ArrowDown" => "↓".to_string(),
            "ArrowLeft" => "←".to_string(),
            "ArrowRight" => "→".to_string(),
            "Tab" => "Tab".to_string(),
            k => k.to_uppercase(),
        };
        if cfg!(target_os = "macos") {
            let mut s = String::new();
            if self.alt {
                s.push('⌥');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.cmd {
                s.push('⌘');
            }
            s + &key
        } else {
            let mut parts = Vec::new();
            if self.cmd {
                parts.push("Ctrl");
            }
            if self.alt {
                parts.push("Alt");
            }
            if self.shift {
                parts.push("Shift");
            }
            parts.push(&key);
            parts.join("+")
        }
    }
}

fn named_str(named: Named) -> Option<&'static str> {
    Some(match named {
        Named::ArrowUp => "ArrowUp",
        Named::ArrowDown => "ArrowDown",
        Named::ArrowLeft => "ArrowLeft",
        Named::ArrowRight => "ArrowRight",
        Named::Tab => "Tab",
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
const SERVER_KEYS: &str = "⌘1…9";
#[cfg(not(target_os = "macos"))]
const SERVER_KEYS: &str = "Ctrl+1…9";

/// Ctrl+1…9 (⌘ on macOS): the nth server on the rail.
pub fn server_number(key: &Key, modifiers: keyboard::Modifiers) -> Option<usize> {
    let Key::Character(k) = key else { return None };
    if !modifiers.command() || modifiers.shift() || modifiers.alt() {
        return None;
    }
    k.parse::<usize>().ok().filter(|n| (1..=9).contains(n))
}

/// "Ctrl+3", or "⌘3" on macOS.
pub fn server_key_label(n: usize) -> String {
    if cfg!(target_os = "macos") { format!("⌘{n}") } else { format!("Ctrl+{n}") }
}

/// The command bound to this key press, if any.
pub fn for_key(key: &Key, modifiers: keyboard::Modifiers) -> Option<Command> {
    ALL.iter().find(|s| s.keys.iter().any(|k| k.matches(key, modifiers))).map(|s| s.command)
}

/// The first key for a command, for menus and hints.
pub fn key_label(command: Command) -> Option<String> {
    spec_of(command).keys.first().map(Chord::label)
}

/// `/split down` → (SplitDown, ""), `/search rust tips` → (Search, "rust tips").
/// The longest matching slash name wins. None for anything else, which goes
/// to the server.
pub fn parse_slash(input: &str) -> Option<(Command, String)> {
    let rest = input.strip_prefix('/')?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut best: Option<(usize, Command)> = None;
    for s in ALL {
        for name in s.slash {
            let n: Vec<&str> = name.split(' ').collect();
            let fits = words.len() >= n.len() && words.iter().zip(&n).all(|(w, n)| w.eq_ignore_ascii_case(n));
            if fits && best.is_none_or(|(len, _)| n.len() > len) {
                best = Some((n.len(), s.command));
            }
        }
    }
    let (len, command) = best?;
    // Commands without arguments only match exactly ("/close it" isn't /close).
    if spec_of(command).args.is_empty() && words.len() > len {
        return None;
    }
    Some((command, words[len..].join(" ")))
}

/// Local slash commands that start with what's typed (for autocomplete):
/// (completed text without the slash, args hint, title).
pub fn complete_slash(input: &str) -> Vec<(String, &'static str, &'static str)> {
    let Some(typed) = input.strip_prefix('/') else { return Vec::new() };
    let typed = typed.to_lowercase();
    // Each command once, by its first name that fits.
    ALL.iter()
        .filter_map(|s| s.slash.iter().find(|name| name.starts_with(&typed)).map(|name| (name.to_string(), s.args, s.title)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(k: &str, cmd: bool, shift: bool) -> Option<Command> {
        let mut m = keyboard::Modifiers::empty();
        if cmd {
            m |= keyboard::Modifiers::COMMAND;
        }
        if shift {
            m |= keyboard::Modifiers::SHIFT;
        }
        for_key(&Key::Character(k.into()), m)
    }

    #[test]
    fn keys_find_their_command() {
        assert_eq!(press("k", true, false), Some(C::QuickSwitcher));
        assert_eq!(press("d", true, false), Some(C::SplitRight));
        assert_eq!(press("D", true, true), Some(C::SplitDown));
        assert_eq!(press("|", true, true), Some(C::ReactLatest));
        assert_eq!(press("k", false, false), None, "plain letters are for typing");
        let alt_up = for_key(&Key::Named(Named::ArrowUp), keyboard::Modifiers::ALT);
        assert_eq!(alt_up, Some(C::PrevChannel));
    }

    #[test]
    fn no_key_is_bound_twice() {
        let chords: Vec<Chord> = ALL.iter().flat_map(|s| s.keys.iter().copied()).collect();
        for (i, a) in chords.iter().enumerate() {
            assert!(!chords[i + 1..].contains(a), "{a:?} is bound twice");
        }
    }

    #[test]
    fn slash_commands_parse() {
        assert_eq!(parse_slash("/split"), Some((C::SplitRight, String::new())));
        assert_eq!(parse_slash("/split down"), Some((C::SplitDown, String::new())));
        assert_eq!(parse_slash("/Search rust  tips"), Some((C::Search, "rust tips".into())));
        assert_eq!(parse_slash("/theme"), Some((C::Themes, String::new())));
        assert_eq!(parse_slash("/close it"), None, "takes no arguments");
        assert_eq!(parse_slash("/join town-square"), None, "the server's");
        assert_eq!(parse_slash("hello"), None);
        assert!(complete_slash("/sp").iter().any(|(n, ..)| n == "split down"));
    }
}
