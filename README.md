# mm-rust

An Omarchy-themed Mattermost client in Rust, with a compatible Rust server to follow.

| Crate | What |
|---|---|
| `crates/omarchy-theme` | Loads the active Omarchy palette and font, and watches for `omarchy theme set` |
| `crates/mm-client` | The iced desktop app (`mm-rust` binary) |
| `crates/mm-api` | Typed Mattermost API v4 REST client and WebSocket event stream (auto reconnect and resume) |

`reference/` (gitignored) holds shallow clones of `mattermost/mattermost` and
`mattermost/desktop`. The OpenAPI spec is in `reference/mattermost/api/v4/source/`.

## Dev server

```sh
dev/seed.sh   # starts mattermost-preview on :8065 with users alice/bob (password Devpass-123!)
MM_URL=http://localhost:8065 MM_LOGIN=alice MM_PASSWORD='Devpass-123!' cargo run -p mm-api --example smoke
```

The same example works against a real server with `MM_TOKEN` (a personal access token).

## Run the client

```sh
cargo run -p mm-client                                   # login screen; the session is saved to ~/.config/mm-rust/session.toml (0600)
MM_URL=http://localhost:8065 MM_TOKEN=<token> cargo run -p mm-client   # skip the login screen
```

Alt+Up / Alt+Down switch channels. Colors and font follow `omarchy theme set` live.
The window class is `mm-rust`, for Hyprland rules.

`dev/drive.sh <pid> <wtype args>` sends keystrokes for UI testing, and refuses unless the app window has focus.

## Tests

```sh
cargo test     # REST against wiremock, WebSocket against a scripted local server, fixtures in fixtures/
```
