# Changelog

All notable changes to Darkmatter. Versions follow [semver](https://semver.org).
The release workflow uses each version's section here as its release notes.

## [Unreleased]

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
