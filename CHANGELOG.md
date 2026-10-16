# Changelog

All notable changes to Darkmatter. Versions follow [semver](https://semver.org).
The release workflow uses each version's section here as its release notes.

## [Unreleased]

- **Plugins:** programs in `~/.config/darkmatter/plugins/` can add slash
  commands, pickers in the **+** menu and icons on a message's hover bar.
  Their answers fill your message box or attach files; you still press send,
  and they never get your login. See [docs/plugins.md](docs/plugins.md).
  `darkmatter plugins` lists them and `darkmatter plugins add` installs one.
- **Example plugins** in `plugins/`: a meme maker (`/meme drake | tabs |
  spaces`, with ImageMagick), Giphy search (with your own API key),
  kaomoji, and Quote.
- **Multi-line message box:** Shift+Enter starts a new line, and the box
  grows with its text (then scrolls). Enter still sends.
- Line breaks in messages now show as line breaks, as in Mattermost.

## [0.2.0] - 2026-10-15

- **Update from the app:** the "new version" notice has an **Update** button,
  then **Restart now**. A copy installed with `install.sh` downloads the
  release, checks it against its SHA256SUMS, and replaces itself. With
  Homebrew it runs `brew upgrade --cask darkmatter`. With pacman it opens a
  terminal for `sudo pacman -Syu`. **What's new** opens the release notes.
- **Slash commands** in the message box and replies, with completions as you
  type. Darkmatter's own (`/split`, `/inbox`, `/theme nord`, `/search`…) run
  locally; the rest go to the server (`/join`, `/msg`, `/away`, plugin
  commands), and their replies show up in the channel.
- **Keyboard shortcuts sheet** (Ctrl+/, `/shortcuts`, or `?`): every command
  with its keys and slash form, searchable.
- **Commands in Ctrl+K:** type `>` and a command's name.
- **☰ menu** with the view settings, Go to, Leave channel, Themes, Keyboard
  shortcuts, About and Log out. The pane title bars and the sidebar footer
  lose their buttons.
- **Hide the sidebar** with Ctrl+B, `/sidebar` or the menu.
- **Compact** now applies to the whole app (sidebar, title bars, composer and
  messages) and is set once, from the menu, Ctrl+Shift+L, `/compact` or `/cozy`.
- **Message actions** (react, reply, edit, delete, mark unread) are icons
  with tooltips, shown on hover right after the name and time, or after the
  text on continued lines and compact rows, instead of at the pane's far edge.
- **+ by every message box**, including replies: emoji (inserted into the
  text), photos and files, pasting an image, mentions and commands.
- **Attachments in replies**, and **Ctrl+V pastes an image** from the
  clipboard as an attachment.
- **@mention and :emoji: completion** as you type (Tab or Enter completes),
  with people from the server and @here, @channel and @all.
- Your own status dot follows `/away`, `/dnd` and `/online`.

**Changed:** compact mode is one app-wide setting now (any per-pane setting
from 0.1 is dropped), and Leave, Themes and Log out moved into the ☰ menu.

## [0.1.1] - 2026-10-15

- **Theme picker:** clicking a theme now uses it. Before, a click only
  previewed it, and closing the picker undid it. Picking a theme always
  applies it right away (Fixed); Auto light/dark is set with its own
  ☀ Light and ☾ Dark buttons. Previously, in Auto (the default on macOS),
  a pick quietly went into a slot and seemed to do nothing.

## [0.1.0] - 2026-10-15

The first release: a Mattermost client for Omarchy and other Linux desktops,
and macOS.

- **Chat:** channels, DMs and group messages with collapsed threads, either
  inline under their message or in a side pane. Markdown, images and files,
  reactions (including custom emoji), editing, deleting, marking unread,
  typing indicators and presence.
- **Tiled panes:** split with Ctrl+D or Ctrl+Shift+D; drag to resize or
  rearrange; the layout is saved per server.
- **Inbox (Ctrl+I):** unread messages from every channel in one pane.
- **Compact mode** per pane, with one line per message.
- **Vim keys** when no text box has focus: j/k, gg/G, r, o, e, dd, +, u.
- **Ctrl+K quick switcher:** channels, people and channels to join.
- **Search (Ctrl+F)** with Jump to the message, recent mentions, and a
  threads inbox.
- **Omarchy themes everywhere:** follows `omarchy theme set` live, all 22
  base themes built in, themes installable from git, and follows the OS
  light/dark setting off Omarchy.
- **Update notice:** when a newer release is out, a note in the sidebar says
  how to update (pacman, Homebrew or install.sh). Turn it off with
  `[updates] check = false`.
- **Login and safety:** email or username plus password (with MFA); the
  login token is kept in the system keyring; a `--read-only` mode for
  looking around a server safely.
