# Darkmatter plugins

A plugin adds slash commands, pickers in the composer's **+** menu, and icons
on a message's hover bar. It's a folder with a `plugin.toml` and a program in
any language. Darkmatter runs the program once per request: one JSON line goes
in on stdin, and JSON lines come back on stdout.

What a plugin can do is deliberately small:

- Its answers only **fill your message box** or **attach a file**. You see
  them and press Enter yourself. Plugins never post.
- Plugins never get your login: they aren't given the session token, and
  `MM_TOKEN`, `MM_PASSWORD`, `MM_LOGIN` and `MM_MFA` are removed from their
  environment.
- They do run as you, on your computer, like any program you install. Only
  install plugins you trust.

## Built in

These ship with Darkmatter and are on unless their programs are missing.
Their source in [`plugins/`](../plugins) is also the place to start when
writing your own.

| Plugin | Adds | Needs |
|---|---|---|
| [`meme`](../plugins/meme) | + Meme picker (`template \| top \| bottom`), `/meme drake \| tabs \| spaces` | python3, ImageMagick (`magick` or `convert`), internet for imgflip's templates |
| [`gif`](../plugins/gif) | + GIF picker, `/gif <search>` | python3 and a free [Giphy API key](https://developers.giphy.com/dashboard/?create=true): send `/gif key <your key>` once |
| [`kaomoji`](../plugins/kaomoji) | + Kaomoji picker, `/kao <name>` | python3 |
| [`quote`](../plugins/quote) | ❝ on the hover bar: quote the message in your reply | python3 |

A built-in plugin whose programs are missing is off; the shortcuts sheet
(Ctrl+/, type "plugin") and `darkmatter plugins` say what it needs. They're
installed in `share/darkmatter/plugins` beside the `darkmatter` binary
(`/usr/share/darkmatter/plugins` with pacman, `~/.local/share/…` with
install.sh) or in `Darkmatter.app/Contents/Resources/plugins`, and updates
replace them.

## Settings

In `~/.config/darkmatter/config.toml`:

```toml
[plugins]
off = ["gif"]          # switch plugins off (built in or yours)

[plugins.gif]          # a plugin's own settings, sent with each request as "config"
api_key = "…"
```

## Installing your own

Your plugins live in `~/.config/darkmatter/plugins/<name>/` (on macOS,
`~/Library/Application Support/darkmatter/plugins/<name>/`). One with the
same name as a built-in plugin replaces it. Restart Darkmatter after adding
one.

```sh
darkmatter plugins                       # what's installed (built in and yours), what's off and why
darkmatter plugins add ./my-plugin       # copy a folder in (shows what it adds, asks first)
darkmatter plugins add https://github.com/you/darkmatter-thing.git
```

To switch one of yours off without removing it, rename its folder to start
with `_` or `.` (or list it in `off`). The keyboard shortcuts sheet (Ctrl+/)
lists every plugin's commands, pickers and actions; type "plugin" in its
filter.

## plugin.toml

```toml
name = "meme"                    # letters, digits, - and _; unique
description = "Caption a meme template"
version = "0.1.0"
run = ["python3", "meme.py"]     # program and arguments, run in the plugin's folder
requires = ["python3", "magick|convert"]   # needed on PATH (`a|b`: either); otherwise it's off

[[command]]                      # /meme …
name = "meme"
args = "<template> | <top> | <bottom>"   # the hint shown while typing
description = "Make a meme"

[[insert]]                       # an item in the + menu that opens a picker
name = "meme"
label = "Meme"
glyph = "🖼"
placeholder = "template | top text | bottom text"

[[action]]                       # an icon on each message's hover bar
name = "quote"
label = "Quote"
glyph = "❝"
```

`run`'s first word runs from the plugin's folder if it names a file there (or
contains a `/`), and from your `PATH` otherwise. Unknown keys are an error, so
typos show up in `darkmatter plugins`.

Darkmatter's own commands come first, then plugins', then the server's: a
plugin can't replace `/split`, and a plugin's `/gif` is used instead of a
server's `/gif`. A command from a plugin that's off isn't sent to the server
either (it could carry a key); Darkmatter says why the plugin is off.

## Requests

Each request is one JSON object with a `type`:

```json
{"type": "command", "name": "meme", "args": "drake | tabs | spaces", "context": {…}}
{"type": "search",  "name": "meme", "query": "drake", "context": {…}}
{"type": "pick",    "name": "meme", "value": "<an item's value>", "context": {…}}
{"type": "action",  "name": "quote", "post": {"id": "…", "message": "…", "user": "bob", "create_at": 1760000000000}, "context": {…}}
```

- `command`: `/name args` was sent.
- `search`: a picker opened (`query` is `""`) or its search box changed.
- `pick`: an item in a picker was chosen.
- `action`: a message's hover icon was clicked.

`context` says where you are:

```json
{
  "team":    {"id": "…", "name": "dev", "display_name": "Dev Team"},
  "channel": {"id": "…", "name": "town-square", "display_name": "Town Square"},
  "thread":  null,
  "me":      "alice",
  "draft":   "what's in the message box now"
}
```

`thread` is the root post's id when the answer is for a thread's reply box.

Every request also has `config`: the plugin's `[plugins.<name>]` settings
from config.toml (`{}` if none).

## Answers

Print one JSON object per line. Each may set any of:

| Field | Does |
|---|---|
| `insert` | Adds text to the end of the message box. |
| `replace` | Replaces the message box's text. |
| `attach` | Attaches a file: an absolute path, or one relative to the plugin's folder. |
| `items` | Picker results: `[{"title": "…", "description": "…", "thumb": "/path/to.png", "value": "…"}]`. With a `thumb` on any item the picker shows a grid of pictures; otherwise a list. `value` comes back in the `pick` request. |
| `notice` | A short note shown in the pane. |
| `error` | Shown as an error. For a slash command, the command's text goes back in the box. |

A `pick` can answer with `items` again to show another level (a category's
contents, say); otherwise the picker closes.

Darkmatter waits up to 30 seconds and reads up to 4 MB. Anything the program
writes to stderr goes to Darkmatter's debug log
(`RUST_LOG=darkmatter=debug`). If it exits with an error and prints no answer,
the last line of stderr is shown.

### Environment

| Variable | Is |
|---|---|
| `DARKMATTER_PLUGIN_DIR` | The plugin's folder (also the working directory). |
| `DARKMATTER_OUTPUT_DIR` | A cache folder for the plugin's files (images it makes, downloads). |
| `DARKMATTER_DATA_DIR` | A folder that's kept, for things like an API key you gave it. |
| `DARKMATTER_VERSION` | Darkmatter's version. |

## A whole plugin

`~/.config/darkmatter/plugins/hello/plugin.toml`:

```toml
name = "hello"
run = ["./hello.sh"]

[[command]]
name = "hello"
args = "<name>"
```

`hello.sh` (executable, uses `jq`):

```sh
#!/bin/sh
name=$(jq -r .args)
jq -nc --arg n "${name:-world}" '{insert: "Hello, \($n)! 👋"}'
```

Try it without Darkmatter:

```sh
echo '{"type":"command","name":"hello","args":"bob","context":{}}' | ./hello.sh
```

## Later

- A sandboxed (WASM) kind of plugin, with permissions you grant, for plugins
  from people you don't know.
- Server plugins on axiond, with the same `plugin.toml` and messages
  (`where = "server"`). See [axiond/PLAN.md](axiond/PLAN.md).
