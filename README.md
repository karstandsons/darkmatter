# mm-rust

An Omarchy-themed Mattermost client in Rust, with a compatible Rust server to follow.

| Crate | What |
|---|---|
| `crates/omarchy-theme` | Loads the active Omarchy palette and font, and watches for `omarchy theme set` |
| `crates/mm-api` | Typed Mattermost API v4 REST client and WebSocket event stream (auto reconnect and resume) |

`reference/` (gitignored) holds shallow clones of `mattermost/mattermost` and
`mattermost/desktop`. The OpenAPI spec is in `reference/mattermost/api/v4/source/`.

## Dev server

```sh
docker run -d --name mm-dev -p 8065:8065 mattermost/mattermost-preview
# open http://localhost:8065, create the first (admin) user, then:
MM_URL=http://localhost:8065 MM_LOGIN=<user> MM_PASSWORD=<pw> cargo run -p mm-api --example smoke
```

The same example works against a real server with `MM_TOKEN` (a personal access token).

## Tests

```sh
cargo test     # REST against wiremock, WebSocket against a scripted local server, fixtures in fixtures/
```
