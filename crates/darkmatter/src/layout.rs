//! The pane layout, saved per server so it comes back next time.
//! Lives in `~/.local/state/darkmatter/layout.toml` (channel ids are per server).

use std::collections::BTreeMap;
use std::path::PathBuf;

use iced::widget::pane_grid::{self, Axis, Configuration, Node};
use serde::{Deserialize, Serialize};

use crate::pane::PaneView;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Saved {
    Split {
        /// "vertical" puts the panes side by side.
        axis: SavedAxis,
        ratio: f32,
        a: Box<Saved>,
        b: Box<Saved>,
    },
    Pane {
        channel: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        inbox: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedAxis {
    Vertical,
    Horizontal,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    servers: BTreeMap<String, Saved>,
}

/// The layout as it is now.
pub fn capture(panes: &pane_grid::State<PaneView>) -> Saved {
    fn walk(node: &Node, panes: &pane_grid::State<PaneView>) -> Saved {
        match node {
            Node::Split { axis, ratio, a, b, .. } => Saved::Split {
                axis: match axis {
                    Axis::Vertical => SavedAxis::Vertical,
                    Axis::Horizontal => SavedAxis::Horizontal,
                },
                ratio: *ratio,
                a: Box::new(walk(a, panes)),
                b: Box::new(walk(b, panes)),
            },
            Node::Pane(p) => {
                let view = panes.get(*p);
                Saved::Pane {
                    channel: view.map(|v| v.channel.clone()).unwrap_or_default(),
                    inbox: view.is_some_and(|v| v.inbox),
                }
            }
        }
    }
    walk(panes.layout(), panes)
}

/// Rebuild a saved layout. Channels `known` rejects (left since, or from
/// another team's data) come back empty, for the caller to fill.
pub fn configuration(saved: &Saved, known: &impl Fn(&str) -> bool, next: &mut usize) -> Configuration<PaneView> {
    match saved {
        Saved::Split { axis, ratio, a, b } => Configuration::Split {
            axis: match axis {
                SavedAxis::Vertical => Axis::Vertical,
                SavedAxis::Horizontal => Axis::Horizontal,
            },
            ratio: ratio.clamp(0.1, 0.9),
            a: Box::new(configuration(a, known, next)),
            b: Box::new(configuration(b, known, next)),
        },
        Saved::Pane { channel, inbox } => {
            let channel = if known(channel) { channel.clone() } else { String::new() };
            *next += 1;
            Configuration::Pane(PaneView { inbox: *inbox, ..PaneView::new(*next - 1, channel) })
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("darkmatter/layout.toml"))
}

fn read() -> File {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn load(server: &str) -> Option<Saved> {
    read().servers.remove(server)
}

pub fn save(server: &str, layout: &Saved) -> Result<(), String> {
    let path = path().ok_or("no state directory")?;
    let mut file = read();
    file.servers.insert(server.to_string(), layout.clone());
    let text = toml::to_string(&file).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Forgets a server's layout (its account was removed).
pub fn forget(server: &str) {
    let Some(path) = path() else { return };
    let mut file = read();
    if file.servers.remove(server).is_some()
        && let Ok(text) = toml::to_string(&file)
    {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml_and_pane_grid() {
        let saved = Saved::Split {
            axis: SavedAxis::Vertical,
            ratio: 0.4,
            a: Box::new(Saved::Pane { channel: "c1".into(), inbox: false }),
            b: Box::new(Saved::Split {
                axis: SavedAxis::Horizontal,
                ratio: 0.5,
                a: Box::new(Saved::Pane { channel: "c2".into(), inbox: false }),
                b: Box::new(Saved::Pane { channel: "gone".into(), inbox: false }),
            }),
        };
        let mut file = File::default();
        file.servers.insert("https://chat.example.com".into(), saved.clone());
        let text = toml::to_string(&file).unwrap();
        let back: File = toml::from_str(&text).unwrap();
        assert_eq!(back.servers["https://chat.example.com"], saved);

        let mut next = 0;
        let config = configuration(&saved, &|c| c != "gone", &mut next);
        let panes = pane_grid::State::with_configuration(config);
        assert_eq!(panes.len(), 3);
        assert_eq!(next, 3, "each pane gets its own widget ids");
        let Saved::Split { b, .. } = capture(&panes) else { panic!() };
        let Saved::Split { b: last, .. } = *b else { panic!() };
        assert_eq!(*last, Saved::Pane { channel: String::new(), inbox: false }, "an unknown channel comes back empty");
    }
}
