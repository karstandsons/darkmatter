# Changelog

All notable changes to Darkmatter. Versions follow [semver](https://semver.org).
The release workflow uses each version's section here as its release notes.

## [Unreleased]

- **With axiond, more of your devices:** a new device of yours shows a
  code; type it in Account → Link a new device on one you already use,
  and the new one reads your encrypted conversations from then on.
- **Safety numbers:** `/verify` (or `/verify @someone`) shows the number
  you and they compare, in person or on a call; verified direct messages
  show ✓. If someone's key changes, their conversations say so, and if
  you had verified them nothing is sent there until you check.
- **Recovery:** Account → Recovery makes a recovery key, or uses a
  passphrase, so losing every device doesn't mean starting over. A new
  device can recover from it, or start over with your password.
- Encryption is marked **beta** until it's been reviewed more.
- **Link previews:** a message's first link shows a card (site, title, a
  few lines, a picture). Mattermost makes its own; with axiond your app
  makes it as you send, so nobody reading it fetches anything, and in an
  encrypted conversation the preview is encrypted with the message.
  `[privacy] link_previews = false` turns it off.
- **Calls (axiond, beta):** 📞 in a channel's title bar (or `/call`)
  starts a call or joins the one going on; direct and group messages ring.
  Voice, video for up to 5 people, and screen sharing, end-to-end
  encrypted (SFrame, RFC 9605) with keys only the devices in the call
  have; everyone sees the same code on the call bar. Pick your microphone, speaker and camera
  under Devices. A call that drops (a new network, a blip) comes back by
  itself, and calls get through networks that block UDP (over the
  server's HTTPS port), also when UDP stops in the middle of one. A call
  that carries nothing for a few seconds shows as reconnecting, and joins
  again if it stays that way; the weak-connection badge shows it too.

## [0.5.0] - 2026-10-21

- **Several servers at once.** Add another Mattermost or axiond server
  with + on the new server rail (or ☰ → Add a server…) and switch with
  Ctrl+1…9, Ctrl+Tab or a click. Each keeps its own layout and stays
  connected in the background: its icon counts your mentions, the window
  title adds up all of them, and notifications say which server they're
  from. Clicking a notification opens the message on its server.
  Right-click an icon to rename, mute, reorder, sign out or remove it.
  Your current login moves over by itself.
- **One inbox for every server:** 📥 at the top of the rail (Ctrl+Shift+I,
  `/all`) lists your unread direct messages and mentions from all your
  servers, newest first; opening one goes to its server and conversation.
- Links to a channel or message on any server you're signed into open in
  Darkmatter, on that server.
- Notifications from encrypted conversations no longer show the message
  (it would stay in the desktop's notification history unencrypted):
  `[notifications] encrypted_previews = true` brings it back.
- **Encrypted text stays put:** sending text copied from an encrypted
  conversation's message box into one that isn't encrypted asks first
  (Enter again sends it). Plugins aren't given the text of encrypted
  conversations unless you tick "May read encrypted conversations" for
  them in the Plugins window, and every plugin request now says which
  server it's for.
- **With axiond:** search finds messages in encrypted conversations too.
  The server can't read them, so Darkmatter keeps its own index of what
  it has decrypted, inside its encrypted database, and shows its finds
  beside the server's.
- **Fixed:** clicking a link in a message did nothing.
- **Fixed:** after attaching files with Ctrl+U, what you typed next went
  nowhere until you clicked the message box.

## [0.4.3] - 2026-10-20

- **Code in the message box.** Enter inside an open ``` block starts a
  new line instead of sending (it used to send an empty code box), and
  Ctrl+Enter (⌘Enter) sends from anywhere. A line above the box says
  which. Pasted code that starts with a path or a `//` comment is sent
  as written, not run as a command. Nothing is autocompleted inside
  code. The + menu has a Code block item.
- **With axiond:** messages in an encrypted conversation that came while
  Darkmatter was closed or offline are read in the order they were sent,
  so a long absence no longer leaves some of them unreadable.

## [0.4.2] - 2026-10-20

- **Typing no longer makes the conversation jiggle.** Pictures and GIFs
  further up changed height for a moment on every keystroke, nudging the
  messages and the scrollbar. They now keep their size.
- **With axiond** (our own server, in development): private channels (`/create private <name>`, then
  `/invite`), end-to-end encrypted like DMs, and encrypted files and
  pictures in them. Pictures are shrunk for the conversation on your own
  computer, since the server can't see them.
  Editing, reactions and @mentions work in encrypted conversations too:
  the server learns who reacted or was mentioned, never the text or the
  emoji.

## [0.4.1] - 2026-10-20

- **GIFs show and play.** A GIF (from `/gif` or any upload) used to show
  only its file name, because Mattermost makes no preview of GIFs. They now
  play in the conversation, and only while they're on screen. The image
  viewer shows the first frame.
- **No message is lost.** Messages wait in an outbox until the server has
  them, through dropped connections, server restarts and closing
  Darkmatter, and are retried without being sent twice. One that's waiting
  shows under your name with why ("can't reach the server").
- **An encrypted local database** for each account (SQLCipher, its key in
  the system keyring). Unsent messages live there now, moved once from the
  old plain file. On Linux, Darkmatter now needs OpenSSL's libcrypto
  (the Arch packages depend on `openssl`).
- Darkmatter names itself to servers ("Darkmatter/0.4.1 (linux)"), so its
  sessions are recognisable in a server's list of signed-in apps.
- **With axiond** (our own server, in development): sign up with an invite
  code, an Account window (signed-in apps, two-factor codes, blocked people,
  deleting your account), ⚑ to report a message or block someone, and the
  first end-to-end encrypted direct messages.

## [0.4.0] - 2026-10-18

- **Image viewer:** clicking an image shows it large over the app instead of
  downloading it and opening another program. Zoom with the wheel, `+`/`-`,
  `0` (fit) and `1` (real size) or a double-click; drag to move; ← → step
  through the conversation's other images; Save… (Ctrl+S) asks where to keep
  a copy. Close with ✕, Esc, `q` or a click beside the image.
- **Plugins window** (Ctrl+Shift+P, `/plugins`, ☰ → Plugins, or + →
  Plugins…): see every plugin and what it adds, switch them on and off, fill
  in settings like the GIF plugin's Giphy key, and install plugins from a git
  URL or a folder (it shows what a plugin adds and runs before installing),
  update or remove them. Changes apply right away.
- Plugins can declare settings (`[[setting]]` in plugin.toml). Secret ones,
  like API keys, are kept out of config.toml, in a file only you can read.
  A setting can fall back to a value the server shares (`from_server`), and
  Save can ask the plugin whether it works (`check`).
- **GIFs without a key of your own:** on a Mattermost server with its GIF
  picker on, the GIF plugin uses the server's Giphy key. Your own key, if you
  set one, still wins. Saving a key in Plugins checks it with Giphy.
- Plugins window: ↑↓ in a settings field or the install box no longer switch
  plugins.

## [0.3.1] - 2026-10-16

- **Built-in plugins:** meme, GIF, kaomoji and Quote now ship with
  Darkmatter and work out of the box (they need python3; meme also
  ImageMagick). One that's missing what it needs is off, and the shortcuts
  sheet and `darkmatter plugins` say why. Updates replace them.
- **GIFs:** get a free Giphy key and send `/gif key <your key>` once.
- **Plugin settings** in config.toml: `[plugins] off = [...]`, and
  `[plugins.<name>]` for a plugin's own settings.

## [0.3.0] - 2026-10-16

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
