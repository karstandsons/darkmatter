# mm-rust

An Omarchy-themed Mattermost client in Rust, with a compatible Rust server to follow.

| Crate | What |
|---|---|
| `crates/omarchy-theme` | Loads the active Omarchy palette and font, and watches for `omarchy theme set` |
| `crates/mm-client` | The iced desktop app (`mm-rust` binary) |
| `crates/mm-api` | Typed Mattermost API v4 REST client and WebSocket event stream (auto reconnect and resume) |

`reference/` (gitignored) holds shallow clones of `mattermost/mattermost` and
`mattermost/desktop`. The OpenAPI spec is in `reference/mattermost/api/v4/source/`.

## Install

```sh
packaging/install.sh               # build and install to ~/.local (binary, launcher entry, icon); no root
packaging/install.sh --uninstall   # remove them again (settings, layout and login are kept)
```

On Arch/Omarchy, `packaging/arch/PKGBUILD` builds a `mm-rust-git` package:
run `makepkg -si` in `packaging/arch`. It then shows up in the app launcher as
**mm-rust**. Add `--read-only` (or `MM_READONLY=1`) to look around a server
without changing anything on it.

## Dev server

```sh
dev/seed.sh   # starts mattermost-preview on :8065 with users alice/bob (password Devpass-123!)
MM_URL=http://localhost:8065 MM_LOGIN=alice MM_PASSWORD='Devpass-123!' cargo run -p mm-api --example smoke
```

The same example works against a real server with `MM_TOKEN` (a personal access token).

## Run the client

```sh
cargo run -p mm-client                                   # login screen; the login token is kept in the system keyring
MM_URL=http://localhost:8065 MM_TOKEN=<token> cargo run -p mm-client   # skip the login screen
```

| Key | Action |
|---|---|
| Ctrl+K | Quick switcher: channels, people (new DM), channels to join |
| Ctrl+F | Search messages |
| Ctrl+Shift+M | Recent mentions |
| Alt+Up / Alt+Down | Previous / next channel |
| Up (empty composer) | Edit your last message (Esc cancels) |
| Shift+Up | Open the thread of the latest message |
| Ctrl+Shift+\\ | React to the latest message |
| PageUp / PageDown | Scroll; older history loads as you reach the top |
| Ctrl+U | Attach files (drag-and-drop works too) |
| Ctrl+T | Theme picker |
| Ctrl+Shift+T | Threads inline or in a side pane |
| Esc | Close switcher, then thread pane; then step out of the message box (vim keys) |

**Panes.** Several channels side by side; the layout is saved per server and
comes back next time.

| Key | Action |
|---|---|
| Ctrl+D / Ctrl+Shift+D | Split right / down |
| Ctrl+W | Close the pane |
| Ctrl+Alt+arrows | Move between panes (or click one); drag a split to resize, a title bar to move |
| Ctrl+I | Inbox: unread messages from every channel in one pane |
| Ctrl+Shift+L | Compact (one line per message) or comfortable, per pane |

**Vim keys**, when no text box has the cursor (Esc leaves the message box):
`j`/`k` move between messages, `gg`/`G` first/last, `h`/`l` panes, `r` reply,
`o` show replies (or open an inbox message), `e` edit, `dd` delete, `+` react,
`u` mark unread, `i` back to typing, `/` search.

The window class is `mm-rust`, for Hyprland rules.

## Themes

mm-rust uses Omarchy's theme format (`colors.toml`) everywhere:

- **On Omarchy** it follows `omarchy theme set` live by default.
- **Elsewhere** (other distros, macOS) it follows the OS light/dark setting: Tokyo Night when dark, Catppuccin Latte when light.
- **Ctrl+T** (or "Themes" in the sidebar) opens the picker. Choose Follow Omarchy, Fixed, or Auto light/dark. Arrow keys preview live, Enter keeps the theme (in Auto it fills the light or dark slot), Esc cancels.
- **All 22 Omarchy base themes are built in**, so they work without Omarchy. Themes installed in Omarchy (`~/.config/omarchy/themes`) show up too.
- **Install from git** in the picker, with the same URLs as `omarchy theme install` (see [omarchy.org/themes](https://omarchy.org/themes/)). Themes go to `~/.config/mm-rust/themes`. On Omarchy, "Also install to Omarchy" runs `omarchy theme install`, which also switches your desktop theme. Only `colors.toml` is read from a theme repo; nothing in it is run.

Settings live in `~/.config/mm-rust/config.toml`:

```toml
[theme]
mode = "auto"              # "omarchy" | "fixed" | "auto"
name = "gruvbox"           # used by "fixed"
light = "catppuccin-latte" # used by "auto"
dark = "tokyo-night"
font = "JetBrainsMono Nerd Font"  # optional; defaults to the system monospace font

[layout]
threads = "inline"         # "inline" | "pane"
density = "comfortable"    # default for new panes: "comfortable" | "compact"

[keys]
vim = true                 # vim-style keys when no text box has the cursor
```

The bundled themes are copied from Omarchy (MIT, see `crates/omarchy-theme/themes/LICENSE`). Refresh them with `dev/sync-omarchy-themes.sh`.
`RUST_LOG=mm_rust=debug` logs UI messages (tokens and login input are never logged).
`--read-only` / `MM_READONLY=1` refuses every write to the server (posting, reacting, marking read…), for safely trying a real server.
The session token is kept in the system keyring (Secret Service / Keychain).

`dev/drive.sh <pid|title=…> <wtype args>` sends keystrokes for UI testing, and refuses unless that window has focus.
`dev/click.sh <pid> <x> <y> [move|click|right|scroll N|drag X2 Y2]` moves and clicks the pointer
inside that window (Hyprland's virtual-pointer protocol, no root), and refuses unless it's focused.
`dev/seed.sh --threads` / `--bulk N` fill the dev server with busy threads or a long channel.

## Tests

```sh
cargo test     # REST against wiremock, WebSocket against a scripted local server, fixtures in fixtures/
```
