# Darkmatter

[![CI](https://github.com/karstandsons/darkmatter/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/karstandsons/darkmatter/actions/workflows/ci.yml)
[![Release](https://github.com/karstandsons/darkmatter/actions/workflows/release.yml/badge.svg?event=push)](https://github.com/karstandsons/darkmatter/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/karstandsons/darkmatter?sort=semver)](https://github.com/karstandsons/darkmatter/releases/latest)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![pacman: karstandsons](https://img.shields.io/badge/pacman-karstandsons-1793d1?logo=archlinux&logoColor=white)](https://github.com/karstandsons/archlinux)
[![Homebrew: karstandsons/tap](https://img.shields.io/badge/homebrew-karstandsons%2Ftap-fbb040?logo=homebrew&logoColor=white)](https://github.com/karstandsons/homebrew-tap)

An Omarchy-themed Mattermost client in Rust, with tiled panes, a unified
inbox and vim keys. It works with any Mattermost server. A compatible Rust
server, **axiond**, is in early development
([crates/axiond](crates/axiond/README.md)).

| Crate | What |
|---|---|
| `crates/omarchy-theme` | Loads the active Omarchy palette and font, and watches for `omarchy theme set` |
| `crates/darkmatter` | The iced desktop app (`darkmatter` binary) |
| `crates/mm-api` | Typed Mattermost API v4 REST client and WebSocket event stream (auto reconnect and resume) |
| `crates/axiond` | The server (`axiond` binary), early: login, channels, DMs, messages, threads, live events |

`reference/` (gitignored) holds shallow clones of `mattermost/mattermost` and
`mattermost/desktop`. The OpenAPI spec is in `reference/mattermost/api/v4/source/`.

## Install

**Arch, CachyOS, Omarchy** (signed pacman repository; updates come with `pacman -Syu`):

```sh
curl -sL https://karstandsons.github.io/archlinux/karstandsons.asc | sudo pacman-key --add -
sudo pacman-key --lsign-key 21F3F82B682C36A4F3576168D033CB9918675079
printf '\n[karstandsons]\nServer = https://karstandsons.github.io/archlinux/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Sy darkmatter
```

**macOS** (Apple Silicon and Intel; updates with `brew upgrade`):

```sh
brew install --cask karstandsons/tap/darkmatter
```

**Other Linux:** download `darkmatter-<version>-<arch>-linux.tar.gz` from the
[latest release](https://github.com/karstandsons/darkmatter/releases/latest),
unpack it and run `./install.sh`. It installs to `~/.local`, with no root needed.

**Updates:** when a new release is out, the sidebar says so, with an **Update**
button. A copy installed with `install.sh` downloads the release, checks it
against the release's SHA256SUMS and replaces itself. On macOS the button runs
`brew upgrade --cask darkmatter`. With pacman it opens a terminal for
`sudo pacman -Syu`. Then **Restart now** starts the new version.

**From source:**

```sh
packaging/install.sh               # build and install to ~/.local (binary, launcher entry, icon); no root
packaging/install.sh --uninstall   # remove them again (settings, layout and login are kept)
```

Or build an Arch package from source with `makepkg -si` in `packaging/arch`
(`darkmatter-git`). However it's installed, Darkmatter shows up in the app
launcher. Add `--read-only` (or `MM_READONLY=1`) to look around a server
without changing anything on it.

## Dev server

```sh
dev/seed.sh   # starts mattermost-preview on :8065 with users alice/bob (password Devpass-123!)
MM_URL=http://localhost:8065 MM_LOGIN=alice MM_PASSWORD='Devpass-123!' cargo run -p mm-api --example smoke
```

The same example works against a real server with `MM_TOKEN` (a personal access token).

## Run the client

```sh
cargo run -p darkmatter                                   # login screen; the login token is kept in the system keyring
MM_URL=http://localhost:8065 MM_TOKEN=<token> cargo run -p darkmatter   # skip the login screen
```

**Ctrl+/** (or `/shortcuts`, or `?` with vim keys) lists every shortcut and
slash command. Ctrl+K with a leading `>` runs any command by name.

**Slash commands** work in the message box and in replies, with completions
as you type (Tab or Enter completes). Darkmatter's own: `/jump [name]`,
`/search [terms]`, `/mentions`, `/inbox`, `/threads`, `/split`, `/split down`,
`/close`, `/compact`, `/cozy`, `/theme [name]`, `/upload`, `/shortcuts`,
`/logout`. Anything else goes to the server, so Mattermost's own (`/join`,
`/msg`, `/away`, `/dnd`, `/shrug`…) and server plugins' commands work too.
Start a message with `//` to send one that begins with a slash.
Typing `@` suggests people and `:ro` suggests emoji, in the same popup. The
**+** by each message box adds emoji, files, a pasted image (Ctrl+V works too),
a mention or a command.

Click an image to see it large inside Darkmatter: scroll or `+`/`-` to zoom,
drag to move around, double-click for real size, ← → for the other images in
the conversation, Save… (Ctrl+S) for a copy, and ✕, Esc or a click beside it
to close. Nothing is downloaded to disk unless you save it.

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
| Ctrl+Shift+P | Plugins window |
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
| Ctrl+Shift+L | Compact (one line per message, tighter spacing everywhere) or comfortable |
| Ctrl+B | Hide or show the channel sidebar |

The **☰ menu** (top left) has the view settings, Go to, Leave channel,
Themes, Plugins, Keyboard shortcuts, About and Log out.

**Vim keys**, when no text box has the cursor (Esc leaves the message box):
`j`/`k` move between messages, `gg`/`G` first/last, `h`/`l` panes, `r` reply,
`o` show replies (or open an inbox message), `e` edit, `dd` delete, `+` react,
`u` mark unread, `i` back to typing, `/` search, `?` shortcuts.

The window class is `darkmatter`, for Hyprland rules.

## Themes

Darkmatter uses Omarchy's theme format (`colors.toml`) everywhere:

- **On Omarchy** it follows `omarchy theme set` live by default.
- **Elsewhere** (other distros, macOS) it follows the OS light/dark setting: Tokyo Night when dark, Catppuccin Latte when light.
- **Ctrl+T** (or "Themes" in the sidebar) opens the picker. Arrow keys preview live; Enter or a click uses that theme from then on (Fixed), and Esc cancels. Auto light/dark has its own ☀ Light and ☾ Dark buttons: click one, then pick the theme for it.
- **All 22 Omarchy base themes are built in**, so they work without Omarchy. Themes installed in Omarchy (`~/.config/omarchy/themes`) show up too.
- **Install from git** in the picker, with the same URLs as `omarchy theme install` (see [omarchy.org/themes](https://omarchy.org/themes/)). Themes go to `~/.config/darkmatter/themes`. On Omarchy, "Also install to Omarchy" runs `omarchy theme install`, which also switches your desktop theme. Only `colors.toml` is read from a theme repo; nothing in it is run.

Settings live in `~/.config/darkmatter/config.toml`:

```toml
[theme]
mode = "auto"              # "omarchy" | "fixed" | "auto"
name = "gruvbox"           # used by "fixed"
light = "catppuccin-latte" # used by "auto"
dark = "tokyo-night"
font = "JetBrainsMono Nerd Font"  # optional; defaults to the system monospace font

[layout]
threads = "inline"         # "inline" | "pane"
density = "comfortable"    # "comfortable" | "compact"
sidebar = true             # the channel list (Ctrl+B)

[keys]
vim = true                 # vim-style keys when no text box has the cursor

[updates]
check = true               # look for a newer release on GitHub at startup and daily (and offer to install it)
```

The bundled themes are copied from Omarchy (MIT, see `crates/omarchy-theme/themes/LICENSE`). Refresh them with `dev/sync-omarchy-themes.sh`.
`RUST_LOG=darkmatter=debug` logs UI messages (tokens and login input are never logged).
`--read-only` / `MM_READONLY=1` refuses every write to the server (posting, reacting, marking read…), for safely trying a real server.
The session token is kept in the system keyring (Secret Service / Keychain).
Darkmatter used to be called mm-rust: on first start it moves the old settings, layout, cache and
keyring login to the new name.

`dev/drive.sh <pid|title=…> <wtype args>` sends keystrokes for UI testing, and refuses unless that window has focus.
`dev/click.sh <pid> <x> <y> [move|click|right|scroll N|drag X2 Y2]` moves and clicks the pointer
inside that window (Hyprland's virtual-pointer protocol, no root), and refuses unless it's focused.
`dev/seed.sh --threads` / `--bulk N` fill the dev server with busy threads or a long channel.

## Plugins

Plugins add slash commands, + menu pickers and message actions. Memes, GIFs
(using the server's Giphy key, or a free one of your own), kaomoji and Quote
come built in. Your
own are small programs in any language that talk JSON; their answers only fill
your message box or attach files. The **Plugins** window (Ctrl+Shift+P,
`/plugins`, or ☰ → Plugins) lists them, switches them on and off, holds their
settings such as API keys, and installs new ones from a git URL or a folder.
See [docs/plugins.md](docs/plugins.md); the
built-in ones in [`plugins/`](plugins) are good starting points.

## Tests

```sh
dev/fetch-webrtc.sh   # once: the libwebrtc that calls (dm-call) link, checked
cargo test --workspace   # REST against wiremock, WebSocket against a scripted local server, fixtures in fixtures/; axiond end to end
```

## License

Darkmatter is open source, under either of

- [MIT](LICENSE-MIT)
- [Apache-2.0](LICENSE-APACHE)

at your option. The server, axiond, is under the
[AGPL-3.0-or-later](crates/axiond/LICENSE). Copyright Karst & Sons. The names "Darkmatter" and "axiond"
aren't covered by the license: see [TRADEMARKS.md](TRADEMARKS.md).
Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md).

Darkmatter is an independent client for Mattermost servers. It isn't affiliated
with or endorsed by Mattermost, Inc.

