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

## Installing

Plugins live in `~/.config/darkmatter/plugins/<name>/` (on macOS,
`~/Library/Application Support/darkmatter/plugins/<name>/`). Restart
Darkmatter after adding one.

```sh
darkmatter plugins                       # what's installed, and any that failed to load
darkmatter plugins add ./plugins/meme    # copy a folder in (shows what it adds, asks first)
darkmatter plugins add https://github.com/you/darkmatter-thing.git
```

To switch a plugin off without removing it, rename its folder to start with
`_` or `.`. The keyboard shortcuts sheet (Ctrl+/) lists every plugin's
commands, pickers and actions; type "plugin" in its filter.

### Examples in this repository

| Plugin | Adds | Needs |
|---|---|---|
| [`kaomoji`](../plugins/kaomoji) | + Kaomoji picker, `/kao <name>` | Python 3 |
| [`quote`](../plugins/quote) | ❝ hover action: quote the message in your reply | Python 3 |
| [`meme`](../plugins/meme) | + Meme picker, `/meme drake \| tabs \| spaces` | Python 3, ImageMagick, internet (imgflip templates) |
| [`gif`](../plugins/gif) | + GIF picker, `/gif <search>` | Python 3, a [Giphy API key](https://developers.giphy.com) in `GIPHY_API_KEY` or `key.txt` |

## plugin.toml

```toml
name = "meme"                    # letters, digits, - and _; unique
description = "Caption a meme template"
version = "0.1.0"
run = ["python3", "meme.py"]     # program and arguments, run in the plugin's folder

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
server's `/gif`.

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
