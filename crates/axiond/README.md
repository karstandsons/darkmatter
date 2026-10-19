# axiond

The Darkmatter chat server. It speaks the part of the Mattermost API that
Darkmatter uses, so Darkmatter logs in to it unchanged. The plan, and why it
exists, is in [docs/axiond/PLAN.md](../../docs/axiond/PLAN.md).

**Status: M0, an early skeleton.** Don't use it for anything you need yet.

## What works

- Logging in and out. Passwords are stored as argon2id hashes and session
  tokens as SHA-256 hashes. After five wrong passwords, an account waits
  before it can try again, and the wait doubles with each try.
- Teams, public channels and direct messages: joining, leaving, and finding
  channels by name.
- Messages: posting, threads, editing and deleting.
- Unread counts and mentions (`@name`, `@channel`, `@all`, `@here`), and
  marking a channel read.
- Live events, so new posts, edits, deletes, typing and read changes show up
  in Darkmatter as they happen. After a dropped connection, Darkmatter picks
  up where it left off.
- Online and offline status.

Anything else Darkmatter asks for gets a clear "axiond doesn't support that
yet". That includes:
- files and pictures;
- reactions;
- search;
- custom emoji;
- the Threads list;
- slash commands on the server;
- invites;
- TLS.

These come in M1.

## Run it

```sh
cargo build --release -p axiond
./target/release/axiond setup --admin you --email you@example.com --team home --team-name "Home"
./target/release/axiond user add friend --email friend@example.com
./target/release/axiond channel add home random
./target/release/axiond          # http://127.0.0.1:8065
```

In Darkmatter, log in to `http://127.0.0.1:8065`.

`setup` and `user add` ask for the password, or read it from
`AXIOND_PASSWORD` in scripts. `axiond user password <name>` sets a new one and
signs that person out everywhere.

## Settings

Settings live in `axiond.toml` (in the working directory, or pass
`--config path`). An `AXIOND_*` environment variable overrides the file.

| Setting | Env | Default | |
|---|---|---|---|
| `listen` | `AXIOND_LISTEN` | `127.0.0.1:8065` | Address and port |
| `data_dir` | `AXIOND_DATA_DIR` | `data` | The database lives here; only axiond's user can read it |
| `behind_proxy` | `AXIOND_BEHIND_PROXY` | `false` | A TLS proxy is in front |
| `site_name` | `AXIOND_SITE_NAME` | `axiond` | The name apps show |

axiond doesn't serve TLS yet, so it **refuses to listen anywhere but
localhost** unless `behind_proxy` is on. Never send passwords over plain HTTP
across a network. Put Caddy (or Traefik, or nginx) in front, for example:

```
chat.example.com {
    reverse_proxy 127.0.0.1:8065
}
```

`AXIOND_LOG=axiond=debug` logs more. Logs never include message text.

## Docker

The image is built from the repository root. It holds one static binary, runs
as a non-root user, and keeps its data in the `/data` volume.

```sh
docker build -f crates/axiond/Dockerfile -t axiond .
docker run -d --name axiond -p 127.0.0.1:8065:8065 -v axiond:/data -e AXIOND_BEHIND_PROXY=true axiond
docker exec -it axiond axiond setup --admin you --email you@example.com --team home
```

## License

AGPL-3.0-or-later (see [LICENSE](LICENSE)). If you run a modified axiond for
other people, you must offer them its source. Darkmatter and the shared
`mm-api` crate stay MIT OR Apache-2.0.
