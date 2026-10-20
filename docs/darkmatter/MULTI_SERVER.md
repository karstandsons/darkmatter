# Darkmatter: several servers at once

Status: planned (2026-10-20). Decided with the user:
- **A server rail** on the left, not tabs along the top.
- **Separate layouts** per server. Panes that mix servers may come later.
- **One account per server.** More than one account on a server may be
  discussed later; nothing here should rule it out.
- **A shared inbox** across servers for DMs and mentions.
- When it's built is left to me (see [Order](#order)).

Darkmatter today signs into one server, Mattermost or axiond. This plan lets
one Darkmatter window be signed into several at once, for example a work
Mattermost and the family's axiond. They are entirely separate servers and
conversations; only the app is shared.

## 1. What people see

### The server rail
- **Where:** a thin column at the far left, beside the sidebar, with one icon
  per server.
- **The icon:** the server's own icon, or its name's initials on the server's
  colour.
- **Badges:**
  - a dot for unread messages;
  - a number for mentions and DMs;
  - a 🔒 corner mark on axiond servers;
  - a warning mark when the server can't be reached or its sign-in has
    expired.
- **Hidden with one server:** the rail appears when the second server is
  added, so one-server use looks as it does now.
- **Ordering:** drag to reorder. Right-click (or the ☰ menu) to rename,
  recolour, mute notifications, sign out or remove.
- **Adding:** the **+** at the bottom opens the sign-in screen in place of the
  current server. Cancel goes back.
- **Keys:**
  - Ctrl+1…9 jumps to the nth server;
  - Ctrl+Alt+↑/↓ moves to the previous or next server;
  - Ctrl+Alt+A jumps to the next server with unread mentions.

  The rail also shows these keys in its tooltips.
- **Commands:**
  - `/server <name>` switches;
  - `/server add` adds one;
  - `/server` alone lists them.
- **Teams:** Mattermost teams stay where they are, in the sidebar header of
  their server. The rail is for servers; teams are inside a server.

### Knowing where you are
Posting to the wrong server is the main risk, and here one server may be
end-to-end encrypted and the other not. So:
- the window title is `Darkmatter: <server name>`;
- the message box and the pane title bars carry the server's colour;
- the rail highlights the current server;
- **pasting from an encrypted conversation into a plain one asks first.**
  Darkmatter remembers what was copied from an encrypted conversation
  (without touching the clipboard itself). If the same text is pasted into a
  conversation on a server that isn't encrypted, it asks once before
  sending. Forwarding a message across servers is never offered.

### Layouts
Each server keeps its own:
- panes and splits;
- open threads;
- drafts and attachments;
- unsent messages;
- sidebar state.

Switching servers swaps the whole main area and keeps the other server's
layout as it was, including scroll positions. App-wide settings stay shared:
- theme;
- density;
- key bindings;
- plugins;
- update checks;
- whether the sidebar is shown.

### The shared inbox (all servers)
- **What:** a top entry on the rail, above the servers, opens a pane listing
  **DMs and mentions from every server**, newest unseen first, each labelled
  with its server (colour and name).
- **Not all messages:** busy channels would drown the rest.
- **Using it:** opening an item switches to that server and that
  conversation. Replying inline from the shared inbox is left for later,
  because the reply box would have to make clear which server it posts to.
- **Each server's own inbox** (Ctrl+I, all unread messages in its teams)
  stays as it is.
- **Keys:** Ctrl+Shift+I opens the shared inbox.
- **What it reads:** it uses what each server already has in memory: unread
  DMs, and mentions from each server's own counts. Each server fetches only
  what it would fetch anyway; there's no extra polling.

### Notifications
- **Every connected server** notifies, not only the one shown.
- **Labelling:** a notification's title starts with the server's name when
  there's more than one server. Clicking it switches to that server and
  conversation.
- **Mute:** each server can be muted on its own. Do Not Disturb and status are
  per server, because they are the server's settings.
- **Unread count:** the window title's mention count is the total over all
  servers that aren't muted. Darkmatter has no tray icon or dock badge
  today; if one comes, it uses the same total.
- **Encrypted servers:** today a notification includes the start of the
  message. For encrypted conversations that puts decrypted text in the
  desktop's notification history, so they get a switch to show only "New
  message from …", on by default.

### Signing in and out
- Signing out of one server ends only that session: its keyring entry, its
  token and its in-memory state. The others stay signed in.
- **Removing a server** also asks whether to delete its local data (the
  encrypted database, cached files). Deleting is the default, because data
  kept quietly is a surprise. Mattermost's desktop app keeps the data;
  their mobile app deletes it.
- An expired sign-in marks that server on the rail. Its conversations stay
  readable from local data, and clicking it opens the sign-in screen for
  that server only.

### Links
- Today every link opens in the browser, apart from `/join`-style channel
  paths, which are looked up on the current server.
- With several servers, a link to a channel or post on any signed-in server
  opens in Darkmatter on that server, by matching the URL against the
  server addresses (Mattermost's desktop app has the same lookup). Other
  links open in the browser.
- A `darkmatter://` URL scheme (deep links from the browser) isn't part of
  this. Darkmatter has no single-instance handling to receive one yet.

## 2. How it's built

### Accounts
- **Data:** `App` holds `accounts: Vec<Account>` and `active: usize` in place
  of one `Screen::Chat`. An `Account` is one server: its `Chat` (or its
  sign-in form, or "loading"), its name, colour and order.
- **Identity:** each account has a stable id (a random UUID made when it's
  added), used for its files and messages. That's what Mattermost's desktop
  app does too: a URL can change, an id doesn't. It also leaves room for
  two accounts on one server later.
- **Messages:** each chat message is wrapped with its account, as
  `Message::Account(id, chat::Message)`. Messages for an account that's
  gone are dropped.
- **Subscriptions:** one websocket per signed-in account
  (`Subscription::run_with` is already keyed by session, so this is a loop).
  Each server reconnects on its own and backs off on its own.
- **Background accounts:**
  - They keep their websocket, store and outbox going: unsent messages are
    retried and encryption handshakes are applied while you're elsewhere.
  - They don't fetch for views nobody is looking at.
  - Their media cache entries are dropped after a while.
  - Their view isn't drawn.

### On disk
- **`accounts.toml`** (config dir) lists the accounts: id, server URL, name,
  colour, order, muted, and which one was last shown. The current
  `session.toml` becomes the first account the first time this version runs.
- **Tokens** stay in the keyring under the server URL (service
  `darkmatter`). With one account per server that's still unique.
  Two accounts per server would key them by account id instead.
- **Layout:** `layout.toml` is already kept per server URL, so each account
  keeps its layout with no change. The shared inbox's place (open or not) is
  saved app-wide beside it.
- **The vault** is already per account: its file name hashes the server and
  user, and so does its keyring entry (`darkmatter-vault`). It stays where
  it is.
- **The old outbox file** (`outbox.json`, keyed by server) is moved into each
  account's vault when that account first starts, as now, one account at a
  time.
- **Opened attachments** move from `cache/darkmatter/files/<file id>/` to
  `cache/darkmatter/accounts/<id>/files/<file id>/`. Two servers can use the
  same file id, and removing an account should remove the decrypted files
  it handed to other apps.
- **App-wide settings** stay where they are: config, themes and plugins.

### What moves where
The [audit](#appendix-what-assumes-one-server) lists every place.
The main ones:
- **Into each account:**
  - the encrypted-file keys in `media.rs`, from step 6: today a global list
    that signing out of any server clears;
  - routing: every async result and websocket event is tagged with its
    account. A resync (`Bootstrapped` while a chat exists) today merges into
    "the" chat; it has to go to its own account.
- **Out of `Chat`, up to the app:** these live in each chat today but belong
  to the person, not the server:
  - plugins;
  - the update checker, its banner and its daily timer;
  - preferences: vim keys, inline threads, compact, the sidebar;
  - overlays: the menu, the shortcuts sheet, the lightbox;
  - recent emoji.
- **Windows with a target account:** the Account, Report and Plugins
  windows each act on "the" chat today. They open for the current account
  and keep acting on it if you switch. The Plugins window shows
  server-provided settings (such as a server's Giphy key) for the current
  account.
- **`Session::load`/`clear`** become a list, and signing out removes one
  entry.
- **`MM_URL` and `MM_TOKEN`** (used in testing) mean "only this server,
  saved nowhere", as now.

### Plugins
- **Context:** a plugin's request gains `server: {id, name, url, kind}`, kind
  being `mattermost` or `axiond`, beside the channel.
- **Shared:** plugins, their settings and secrets stay shared by every
  account: they're yours, not a server's. Settings a server provides come
  from the server the request is for.
- **Encrypted conversations:** plugins already never get the token. A plugin
  asked about a post in an encrypted conversation gets the decrypted text
  only if the user turned that on for the plugin (off by default), because a
  plugin can send things anywhere.

### Encryption (axiond)
Nothing in the protocol changes: each axiond account already has its own
device, vault and keyring key (`darkmatter-vault`, keyed by account). Two
axiond servers are two devices with separate identities, and their keys never
meet.

## 3. What we might be missing (research notes)

**Mattermost's desktop app** ([multiple
workspaces](https://docs.mattermost.com/end-user-guide/preferences/connect-multiple-workspaces.html),
[configuration](https://docs.mattermost.com/developers/contribute/more-info/desktop/architecture/configuration)):
- **Servers:** they're listed by name, URL and order in `config.json`, each
  with a UUID. The rest of the app identifies a server by that UUID.
- **Admin-provided servers:** a build config or Windows Group Policy can
  pre-define servers, which users can't edit or remove. We don't need this
  now. If a company ever ships Darkmatter, a system-wide `accounts.toml`
  (e.g. `/etc/darkmatter/`) would cover it.
- **Matching links:** a lookup matches any URL to a server, for deep links and
  links between servers. Our [Links](#links) is the same idea.
- **Permissions per server:** microphone, camera and notifications. For us,
  that becomes per-server notifications now, and the camera and microphone
  when calls come.
- **Removing a server** keeps its data, so it can be added back.
- **Tabs and windows:** since 6.0 several tabs can be open at once and pulled
  into their own windows. That's the "panes across servers" idea, deferred.

**Mattermost's mobile app** (v2, [multi-server
spec](https://mattermost.atlassian.net/wiki/spaces/MSU/pages/1053327503/Multi-Server+Support),
[v2 announcement](https://mattermost.com/blog/mattermost-mobile-v2-generally-available/)):
- **Badge:** the servers button shows unreads and mentions summed over every
  server *except the current one*. That's a good rule for our rail's top
  entry.
- **Removing:** removing a server deletes its data, and the app switches to
  the last server viewed that's still signed in.
- **Beta problems:** their beta had inaccurate badge counts with several
  servers. Counts should come from each server's own unread state, never be
  added up twice, and be tested with several servers.

**Slack** ([new design](https://slack.com/intl/en-au/help/articles/16764236868755-An-overview-of-Slacks-new-design),
[switcher](https://tidbits.com/2023/09/21/how-to-restore-the-slack-workspace-sidebar/)):
- **The rail:** the workspace rail can be hidden (Ctrl+Shift+S), and a badge
  shows new content per workspace.
- **Home:** it can be filtered by workspace, and Activity gathers mentions and
  replies. That's the shape of our shared inbox: an "all" view filtered by
  server.

**Matrix clients** ([Ubuntu's
guide](https://documentation.ubuntu.com/project/community/contributors/matrix/multiple-accounts)):
- **Element:** it still can't hold two accounts at once; people run separate
  profiles (`--profile`).
- **Why it's hard for an encrypted client:** each account has its own crypto
  store and device. We already keep those per account, so we avoid
  Element's problem.

**Things these raise that the plan now covers:**
- stable account ids rather than URLs;
- deleting data on removal, and asking first;
- the badge rule;
- a sign-in that expired on a server you aren't looking at;
- notifications labelled and routed by server;
- links routed by server;
- keeping text from leaking from encrypted to plain conversations;
- plugin context;
- per-server Do Not Disturb.

**Still open, for later:**
- Panes showing conversations from different servers side by side.
- Replying from the shared inbox.
- More than one account on one server.
- Phones: the [mobile plan](../axiond/research/mobile-apps.md) should take
  the same model: an account list, data per account, notifications routed by
  account.

## 4. Testing
- **Unit tests:**
  - the account list and moving the old files over (session, layout);
  - routing messages to the right account;
  - badge sums (all servers but the current one);
  - the shared inbox merging two stores;
  - link matching;
  - signing out of one account leaving the other untouched, including its
    keyring entry and its file keys.
- **In the app:**
  - two sandbox servers at once, a local Mattermost (mm-dev) and a sandbox
    axiond, in one Darkmatter;
  - messages arriving on both;
  - notifications from the one in the background;
  - the outbox delivering on the background server after a restart;
  - signing out of one.

  Never the real server.

## Order
1. **Now (with M2 step 7):** stop making new things app-wide. *(done: the
   step 6 file keys now live in each account's encryption state.)*
2. **After M2 step 7:** the work above, as **Darkmatter 0.5.0**, in these
   commits:
   1. accounts, moving the old files over, routing messages to accounts,
      background accounts;
   2. the rail, its keys and badges, adding and removing;
   3. notifications and links by server, mute per server;
   4. the shared inbox;
   5. plugin context and the encrypted-to-plain paste check.

   It comes before M2 steps 9 and 10 because those add per-account screens
   (devices, recovery), which are easier to build once there are accounts.

## Appendix: what assumes one server
From an audit of `crates/darkmatter` (2026-10-20). Paths are under
`crates/darkmatter/src/`.

**Global state**
- `media.rs`, `KEYS`: encrypted-file keys for the whole process. *(Moved
  into each account's encryption state with M2 step 7.)*
- `session.rs`, `READ_ONLY`, and `style.rs`, `FONT`: rightly app-wide.

**On disk**
- `session.rs`, `session.toml`: one saved login. Becomes `accounts.toml`.
- `layout.toml`: already per server URL.
- `config.toml`: app-wide, stays.
- The vault and its keyring entry: already per account.
- `outbox.json` (legacy): keyed by server; moved in per account.
- `cache/darkmatter/files/<file id>`: needs the account.
- Pasted images: app-wide, fine.
- Plugins' folders and secrets: app-wide, by design.

**Keyring**
- `darkmatter` holds tokens by server URL (one account per server).
- `darkmatter-vault` holds keys by server and user.

**App and sessions**
- `app.rs`: `Screen` holds one chat. The login form replaces everything.
- `Session::load`, `Session::clear`, `LoggedIn` and `save` handle a single
  login.
- `Bootstrapped(Ok)` resyncs into the one chat.
- The Account, Report and Plugins windows act on the current chat.
- `subscription()`: one websocket plus the update, layout-save and 1-second
  timers, all from the one chat. `ws_events` is already keyed by session but
  its events aren't tagged.

**Messages**
- No message carries an account: `Ws`, `Media`, `Animation`, `History`,
  `Delivered`, `SearchResults`, `ThreadsLoaded`, `InboxOpen` and the rest.
  This is the largest change.

**Notifications and title**
- `chat.rs`, `notify_mention`: no server name, and a click can't say which
  server it's for.
- `App::title`: mention count and channel from the one chat.
- No tray, dock badge or urgency hint exists.

**Links**
- `Message::Link` opens everything in the browser. `goto_location` resolves
  `/channels/<name>` on the current server.
- No deep links, URL scheme or single-instance handling.

**Inbox, threads, search, switcher**
- `Store::inbox(team)`: the current team only. The shared inbox merges stores
  and filters to DMs and mentions.
- Threads and search are per team, which is fine per account.
- Ctrl+K reads one store. It searches across servers with results tagged.

**Plugins**
- `Chat.plugins`: loaded per chat. Moves to the app.
- `plugins.rs`, `Context`: no server. Add one.
- `PluginTarget`: routed by channel only. Add the account.

**Media**
- `media::Key`: no server, but the cache is per chat, so it stays per account
  and only the messages need tagging.

**Crypto**
- Per account, `KEYS` included since M2 step 7.
