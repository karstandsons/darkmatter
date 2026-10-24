# axiond

The Darkmatter chat server. It speaks the part of the Mattermost API that
Darkmatter uses, so Darkmatter logs in to it unchanged. The plan, and why it
exists, is in [docs/axiond/PLAN.md](../../docs/axiond/PLAN.md).

**Status: M1, a foundation to try out.** Everything Darkmatter does on
Mattermost works, plus HTTPS, invites, two-factor codes, backups and the
deploy kit. Messages aren't end-to-end encrypted yet (that's M2), so don't
use it for anything secret yet.

## What works

- Logging in and out. Passwords are stored as argon2id hashes and session
  tokens as SHA-256 hashes. After five wrong passwords, an account waits
  before it can try again, and the wait doubles with each try.
- Two-factor login codes (any authenticator app), each usable once. If
  someone loses their phone, `axiond user mfa-off <name>` turns them off.
- A list of your signed-in apps, with signing one or all of them out.
- Joining by invite only: `axiond invite` makes a code (one use, a week, by
  default). Admins can also deactivate people (`axiond user deactivate`),
  and anyone can delete their own account; `axiond user delete` does it for
  someone else.
- Admin commands that sign someone out (`user deactivate`, `delete`,
  `password` and `mfa-off`) are done by the running server, through
  `admin.sock` in the data folder, so that person's open apps and calls
  are cut off at once. With no server running, the command does it
  itself. The server also checks every 5 seconds that each open
  connection and call still has a session.
- Blocking someone (no direct messages either way, no notifications from
  them) and reporting messages or people to the admins (`axiond reports`).
- An audit log of what admins did (`axiond audit`).
- Rate limits: every address gets 300 requests at once and 30 a second
  after that; logging in and signing up get 10, then one every 6 seconds;
  each person can send 30 messages at once, then 5 a second. Behind a proxy,
  the address is the last one in `X-Forwarded-For`.
- Teams, public channels and direct messages: joining, leaving, and finding
  channels by name.
- Messages: posting, threads, editing, deleting and reactions.
- Calls (in progress, M3): one per channel, through a small SFU built in
  (str0m) on one UDP port; joining from another device moves you there,
  and leaving the channel or signing out takes you out. Media is encrypted
  to the server only so far, so Darkmatter keeps calls behind a build
  feature until they're end-to-end encrypted.
- Files and pictures, up to 100 MB each (`max_file_mb`). Pictures get a
  thumbnail and a preview. Files are stored under random names and only ever
  served as downloads.
- The Threads list: you follow a thread when you start it, reply or are
  mentioned, with unread replies and mentions per thread.
- Custom emoji (anyone can add one; `axiond emoji add name file.png` too)
  and profile pictures, redrawn as a 256-pixel PNG so nothing hidden in the
  original (like where a photo was taken) is kept.
- Search, with Mattermost's syntax: words, `"phrases"`, `prefix*`, `-word`,
  `from:`, `in:`, `before:`, `after:` and `on:`. Encrypted channels are never
  searched on the server.
- Unread counts and mentions (`@name`, `@channel`, `@all`, `@here`),
  marking a channel read, and "mark as unread".
- Live events, so new posts, edits, deletes, typing and read changes show up
  in Darkmatter as they happen. After a dropped connection, Darkmatter picks
  up where it left off.
- Online and offline status, plus away, do not disturb and appear offline
  set by hand.
- Slash commands on the server: `/join`, `/leave`, `/msg`, `/me`, `/shrug`,
  `/away`, `/online`, `/dnd`, `/offline`, `/header`, `/purpose`, `/invite`,
  `/kick`, `/mute` and `/help`, with completion as you type.

Anything else Darkmatter asks for gets a clear "axiond doesn't support that
yet".

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
| `data_dir` | `AXIOND_DATA_DIR` | `data` | The database and uploaded files live here; only axiond's user can read it |
| `behind_proxy` | `AXIOND_BEHIND_PROXY` | `false` | A TLS proxy is in front |
| `site_name` | `AXIOND_SITE_NAME` | `axiond` | The name apps show |
| `max_file_mb` | `AXIOND_MAX_FILE_MB` | `100` | The largest upload, in MB |
| `domain` | `AXIOND_DOMAIN` | | Serve HTTPS for this domain, with a Let's Encrypt certificate |
| `acme_email` | `AXIOND_ACME_EMAIL` | | Where Let's Encrypt sends certificate notices |
| `acme_staging` | `AXIOND_ACME_STAGING` | `false` | Use Let's Encrypt's test service |
| `acme_directory`, `acme_ca` | `AXIOND_ACME_DIRECTORY`, `AXIOND_ACME_CA` | | Another ACME server, and a CA file to trust for it (for testing with Pebble) |
| `tls_cert`, `tls_key` | `AXIOND_TLS_CERT`, `AXIOND_TLS_KEY` | | Serve HTTPS with your own PEM files |
| `backup_hours` | `AXIOND_BACKUP_HOURS` | `24` | Back up into `data_dir/backups` this often (0 = never) |
| `backup_keep` | `AXIOND_BACKUP_KEEP` | `7` | How many of those to keep |
| `calls_udp` | `AXIOND_CALLS_UDP` | a free port on `listen`'s address | The one UDP port call media uses, e.g. `0.0.0.0:443` |
| `calls_public_ip` | `AXIOND_CALLS_PUBLIC_IP` | | The address devices send call media to. Needed when `calls_udp` is `0.0.0.0` (calls are off without it) |
| `calls_public_port` | `AXIOND_CALLS_PUBLIC_PORT` | the bound port | The port devices send call media to, when it's forwarded to another |

### HTTPS

axiond **refuses to serve plain HTTP anywhere but localhost**: passwords and
messages would cross the network readable. Choose one:

- **Let's Encrypt, built in:** set `domain` and listen on port 443
  (`listen = "0.0.0.0:443"`). axiond gets its certificate itself (TLS-ALPN-01,
  so only port 443 needs to be open) and renews it. The certificate is kept
  in `data_dir/acme`. Try `acme_staging = true` first: Let's Encrypt limits
  how often you can ask for real certificates. (Not yet tried against a real
  domain; please report how it goes.)
- **Your own certificate:** set `tls_cert` and `tls_key`.
- **A proxy in front** (Caddy, Traefik, nginx) with `behind_proxy = true`.
  With Caddy:

  ```
  chat.example.com {
      reverse_proxy 127.0.0.1:8065
  }
  ```

Either way, responses carry `Strict-Transport-Security`, so apps and browsers
never fall back to plain HTTP.

`AXIOND_LOG=axiond=debug` logs more. Logs never include message text.

## Backups

`axiond backup` writes a `.tar.gz` of the database and every stored file
(uploads, emoji, pictures, certificates), safe to take while the server runs.
The server also does this by itself every `backup_hours`, and keeps a copy of
the database before any upgrade changes it.

Backups on the same disk don't survive losing the server. Copy them
somewhere else too, encrypted (restic does both).

To restore, stop axiond and run `axiond restore <file>` with an empty (or
new) data folder. It never writes over an existing database.

## Docker

For a real server, use the deploy kit in [deploy/axiond](../../deploy/axiond):
Docker Compose with HTTPS built in, a hardening checklist, and encrypted
backups off the server.

The image is built from the repository root. It holds one static binary, runs
as a non-root user, and keeps its data in the `/data` volume.

```sh
docker build -f crates/axiond/Dockerfile -t axiond .
docker run -d --name axiond -p 127.0.0.1:8065:8065 -v axiond:/data -e AXIOND_BEHIND_PROXY=true axiond
docker exec -it axiond axiond setup --admin you --email you@example.com --team home
```

## Releasing

Releases are tagged `axiond-vX.Y.Z`; the `axiond release` workflow builds
and publishes them. For each one:

1. Set `version` in `crates/axiond/Cargo.toml` to `X.Y.Z`.
2. Make its upgrade fixture, from that commit:

   ```sh
   AXIOND_FIXTURE=X.Y.Z cargo test -p axiond --test upgrade make_upgrade_fixture -- --ignored
   ```

   It runs a server with plain and encrypted conversations in it (a DM, a
   private channel, a public channel turned encrypted, edits, reactions, a
   linked device, a signed-out one, backups, a link request) and writes
   `tests/fixtures/axiond-X.Y.Z.db` and `axiond-X.Y.Z.devices.json` (the
   devices' sessions and saved state). Test passwords and keys only.
3. Add `X.Y.Z` to the end of `UPGRADE_FIXTURES` in `tests/upgrade.rs`, and
   check `cargo test -p axiond --test upgrade` passes.
4. Commit the version and the fixture, then push the tag `axiond-vX.Y.Z`.

From then on, every test run upgrades that release's database to the
current code and checks the same devices still read what it held. If an
upgrade test fails, fix the migration, never the fixture: real servers
hold that data. Fixtures named `-dev.N` were made before the first
release, at migration N.

## License

AGPL-3.0-or-later (see [LICENSE](LICENSE)). If you run a modified axiond for
other people, you must offer them its source. Darkmatter and the shared
`mm-api` crate stay MIT OR Apache-2.0.
