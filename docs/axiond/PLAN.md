# axiond: server plan

axiond is the Darkmatter chat server. This is a plan, not a spec: each section states a decision and why, and the open questions are at the end.

## Who it's for and why

- **Our own team**, moving off Mattermost and Slack.
- **Friends and small groups** who want a chat server that's easy to run on almost anything: a home server exposed to the internet, a cheap VPS, a NAS. It's one Docker container with a small footprint.

Why switch:
- **Mattermost:** the free editions are getting smaller.
  - Mattermost v11 (announced October 2025) introduced **Entry** as the default free edition. Its planned limits include a 10,000-message history cap in the UI and search, 10,000 push notifications a month, and 40-minute 1:1 calls ([Mattermost blog](https://mattermost.com/blog/introducing-mattermost-entry/), [FAQ](https://docs.mattermost.com/product-overview/faq-general.html)).
  - Team Edition keeps full history but has fewer features. It also loses GitLab SSO in v11.
  - We want the whole feature set and the freedom to change it.
- **Slack:** cost and lock-in. Free and Pro exports cover public channels only ([Slack help](https://slack.com/help/articles/201658943-Export-your-workspace-data)).
- **Security and privacy are a top priority.** Conversations shouldn't be easy to intercept or read, even by whoever runs the server.

## Goals

1. **It replaces Mattermost and Slack for a team.** That covers:
   - channels, DMs and group DMs;
   - threads, reactions, files, custom emoji;
   - search, mentions and unreads;
   - slash commands, bots and webhooks;
   - presence and typing.
2. **End-to-end encryption by default** wherever it doesn't cost a feature the channel needs (see [E2EE](#end-to-end-encryption)).
3. **Small and simple to run.**
   - One static binary and one Docker image (amd64 and arm64).
   - SQLite by default, so there are no other services to run.
   - Idle RAM under 50 MB.
   - A first-run setup page.
4. **Darkmatter is the main client on every platform:** Linux and macOS now; iOS, Android and Windows later.
5. **Migration in:** import from a Mattermost bulk export and a Slack export zip.

## Non-goals (for now)

- **Full Mattermost API compatibility.** We implement the part Darkmatter uses (see [Appendix A](#appendix-a-mattermost-api-surface-darkmatter-uses-today)) so today's Darkmatter runs on axiond from the start. We don't chase the Mattermost web app or the Mattermost mobile apps. Their API is several hundred endpoints, and they would never speak our E2EE extension anyway.
- **Federation** between servers. Maybe later (M5+). MLS doesn't rule it out.
- **Huge installs.** The target is 2 to a few hundred users per server. Postgres and multiple nodes are possible later, but they don't drive the design.
- **Voice and video** at launch. Calls are a later project. WebRTC with SFrame/MLS keys is a candidate, but nothing is decided.

## The central tension: E2EE vs server features

If the server can't read messages, it can't:
- search them;
- unfurl links;
- run server-side bots, slash commands or plugins over their content;
- put message text in push notifications.

Every encrypted chat app makes this trade-off. Our answer is to choose **per channel**, with safe defaults:

| Conversation | Default | Why |
|---|---|---|
| DMs and group DMs | **E2EE, always** | These are the private conversations. Nothing server-side needs to read them. |
| Private channels | **E2EE on**; an admin can turn it off when a channel was created for a bot or integration | Small, closed membership is where MLS works best. |
| Public/team channels | **Off by default; a channel admin can turn it on** | Many members, people joining late who need history, bots and server search. Teams that want it can turn it on. |

- Channels show their state clearly: a lock icon and "End-to-end encrypted" in the header.
- **Turning encryption on applies to new messages only.** Older history stays as it was. Turning it off again needs an admin and posts a visible system message.
- **What we keep in encrypted channels:**
  - Client-side search over a local index on each device.
  - Link previews fetched by the sender's client. This is opt-in, because fetching reveals the sender's IP to the linked site.
  - Bots that are members of the MLS group. A bot is a device that holds keys, so it can read only what it's been invited to.

## End-to-end encryption

### Protocol: MLS (RFC 9420) via **openmls**

**Why MLS:**
- It's the IETF standard for group messaging ([RFC 9420](https://www.rfc-editor.org/rfc/rfc9420), July 2023).
- It's built for groups. Adding or removing members costs O(log n), so private channels with dozens of members are practical.
- It gives forward secrecy and post-compromise security.
- Each device is a group member, which gives us multi-device support for free.

**Why openmls:**
- It's MIT-licensed, compatible with both the client's MIT/Apache license and AGPL.
- It's actively released (0.9.1 on crates.io, October 2026).
- It's pure Rust, so the same code runs in Darkmatter on desktop and mobile.

**Alternatives we considered:**
- **mls-rs** (AWS; Apache-2.0 OR MIT; 0.56): a good second choice. We'd switch only if openmls stalls.
- **vodozemac** (Olm/Megolm, as Matrix uses; Apache-2.0; audited by Least Authority): mature, but Megolm's group model is weaker than MLS for removing members, and it's a different design lineage.
- **libsignal:** **AGPL-3.0** and not published as a general library. It's built for Signal's own server, and its license would force the client to be AGPL. Rejected.

We do **not** write our own crypto. The `dm-crypto` crate (below) is a thin layer: storage, the wire format, and mapping channels to MLS groups.

### How it fits the API

- **Encrypted posts** go through the same `POST /posts` endpoint:
  - `message` is empty or holds a placeholder such as "🔒 Encrypted message";
  - the ciphertext travels in `props.dm_e2ee`: `{v, group_epoch, ciphertext}`, base64.
  - Old or foreign clients see the placeholder and nothing breaks.
- **Edits, reactions and files are encrypted too:**
  - Edits are new ciphertext.
  - Reactions in E2EE channels are encrypted application messages. The server sees only that something happened, not which emoji.
  - Files are encrypted on the client with a random key, uploaded as opaque blobs, and the key travels inside the encrypted post.
- **New endpoints under `/api/v4/e2ee/…`, plus websocket events `mls_welcome` and `mls_commit`:**
  - key packages: upload, and claim one per device;
  - delivering MLS handshake messages (commits, proposals, welcomes) in order, per group;
  - the device list per user.
- The server is the MLS **Delivery Service**: it orders commits and stores messages, but can't read them. Membership changes in the channel (join, leave, kick) trigger an MLS add or remove. The client of whoever made the change sends the commit; the server enforces that membership matches.

### Devices, verification, recovery

- **Each device has its own identity key.** The devices page lists them; you can rename or remove a device. Removing one also removes it from every group.
- **Verification:**
  - Safety numbers or a QR code between two users, as Signal does.
  - Logging in on a new device requires approval from an existing device, or the recovery key.
  - A changed key shows a warning in the conversation.
- **Key backup.** This is the hard usability problem.
  - Without a backup, a new device can't read old messages.
  - Plan: an optional **encrypted history backup** stored on the server, encrypted with a key derived from a **recovery key** (shown once and saved by the user) or a passphrase run through argon2id.
  - The server stores only ciphertext.
  - With no backup, a new device sees messages from the moment it joins. That's the Signal model, and it's acceptable.

### What the server still sees (metadata)

**We say this openly in the docs. The server knows:**
- who is in which channel;
- who sent a message, when, and roughly how big it is;
- IP addresses (kept only in short-lived memory or logs; see below);
- reaction and edit events, but not their content;
- file sizes.

**It doesn't know:** message text, file contents, emoji used, link targets or edits.

**Reducing metadata further** (sealed sender, padding) is a later hardening item, not v1.

## Security baseline (all phases)

- **TLS always.** Either:
  - built-in ACME with `rustls-acme` (0.15, Apache-2.0 OR MIT; TLS-ALPN-01 by default, HTTP-01 when behind a proxy), so `axiond --domain chat.example.com` gets its own certificate; or
  - run behind Caddy, Traefik or nginx with `--behind-proxy`.

  Plain HTTP is allowed only on localhost, for development. HSTS is on.
- **Passwords:** argon2id, with parameters tuned to the box at first run.
- **Two-factor:** TOTP, and **passkeys/WebAuthn** (also usable as the only sign-in). Admins can require 2FA.
- **Sessions:**
  - per-device tokens, hashed at rest;
  - a list of active sessions with revoke ("log out everywhere");
  - short-lived access tokens plus refresh tokens on mobile.
- **Rate limits** on login, signup, password reset and posting, per IP and per account. Repeated failed logins lock the account out with exponential backoff.
- **Files:** E2EE files are already ciphertext. For everything else there's optional server-side encryption at rest with a key in the config, not in the database.
  - We store blobs under random IDs, never under user filenames.
  - Uploads are served with `Content-Disposition: attachment` and a strict CSP. They're never sniffed as HTML.
- **Database:** optional SQLCipher for SQLite at rest. Postgres users rely on disk encryption.
- **Privacy defaults:**
  - **no telemetry**, ever;
  - minimal logs: no message bodies, IPs kept in memory for rate limiting only;
  - access logs off by default.
- **An audit log** for admin actions: role changes, user deactivation, turning E2EE off, exports.
- **Supply chain:**
  - `cargo-deny` (licenses and advisories) and `cargo-audit` in CI;
  - reproducible release builds;
  - signed images, using cosign or GitHub attestations.
- **Disclosure:** `security.txt`, `SECURITY.md` with a private reporting address, and a published threat model (next section).
- **Dependency hygiene:** a small dependency tree, `#![forbid(unsafe_code)]` in axiond's own crates.

### Threat model (summary; the full version goes in `docs/axiond/THREAT_MODEL.md`)

| Adversary | Protected? |
|---|---|
| Someone on the network (coffee-shop Wi-Fi, ISP) | Yes. TLS everywhere. |
| A server operator or hosting provider reading conversations | **For E2EE channels, yes:** they get ciphertext and metadata only. For non-E2EE channels, no. That's stated in the UI. |
| A stolen database or backup | E2EE content is safe. Passwords are argon2id hashes. Session tokens are hashed. Non-E2EE content is exposed unless at-rest encryption is on. |
| A compromised server serving malicious keys | Mitigated by device verification and warnings when a key changes. Unverified contacts are a risk, as in Signal. |
| A stolen unlocked device | Not protected. Device removal limits the damage to new messages afterwards. |
| A malicious plugin | Server plugins run sandboxed, can't read E2EE content, and get only the permissions they're granted. |
| Push providers (Apple, Google) | They see only that a ping was sent: no content, no sender (see [Mobile](#mobile-and-push)). |

## Architecture

```
crates/
  mm-api/      shared Mattermost-compatible types + client (MIT/Apache) — server reuses the models
  dm-crypto/   NEW: MLS wrapper, device keys, wire format, backup format (MIT/Apache)
  dm-plugin/   NEW: plugin.toml + JSON protocol types, shared by client and server (MIT/Apache)
  axiond/      NEW: the server (AGPL-3.0-or-later)
  darkmatter/  client
```

- **Language: Rust.**
  - It shares the `mm-api` models and `dm-crypto` with the client, so the two can't drift apart.
  - It produces one static binary and keeps memory low, which suits a small footprint.
  - The team already writes Rust.
- **Stack:**
  - **axum** on **tokio** for REST and websockets;
  - **sqlx** for the database, with compile-time-checked queries;
  - **tower** middleware for rate limiting, auth and tracing;
  - **rustls** for TLS.
- **Database:**
  - **SQLite by default**, in WAL mode: one file and nothing to administer. It's plenty for hundreds of users.
  - **Postgres optional** for bigger installs. The same queries are kept portable, and both run in CI.
- **Search:** SQLite FTS5 (or Postgres `tsvector`) for non-E2EE channels. Encrypted channels are searched on the client.
- **Real-time:**
  - an in-process broadcast hub;
  - websocket events with sequence numbers and replay on reconnect, matching what Darkmatter's `ws.rs` already does with `connection_id` and `sequence_number`.

  Multiple nodes later need Postgres LISTEN/NOTIFY or a small pub/sub, but that's not v1.
- **Files:** local disk by default (`/data/files`). S3-compatible storage is optional (MinIO, R2, Backblaze). Thumbnails and previews are generated on upload, for non-E2EE files only; E2EE previews are made by the client.
- **Backups:**
  - A built-in `axiond backup` command takes an online SQLite snapshot (`VACUUM INTO`) plus the files directory and writes a tarball.
  - An optional schedule keeps the last N backups.
  - Restoring is documented.
  - Litestream works next to it for people who want streaming replication.
- **Config:** `axiond.toml` plus `AXIOND_*` environment variables (env wins). Secrets can come from files (`*_FILE`), as Docker secrets do.
- **First run:** the first visit, or `axiond setup` in a terminal, creates the admin account and team. Signup is invite-only by default.
- **Docker:**
  - A `FROM scratch` (or distroless) image containing a static musl binary, CA certificates, and a `/data` volume.
  - Multi-arch (amd64, arm64), built by the existing release workflow with an `axiond-v*` tag.
  - Health check at `/api/v4/system/ping`.
- **Footprint target:** idle RAM under 50 MB, a binary under 30 MB, and running comfortably on a Raspberry Pi 4 or a $5 VPS.

## Mobile and push

- **Push works by wake-up only.** The notification carries no message text, no sender and no channel. It is just "you have something new", plus an opaque ID. The app wakes, fetches over TLS, decrypts, and builds the notification on the device.
  - Signal and Wire describe the same design.
  - A 2024 PETS study found most messaging apps leak metadata or content through FCM payloads ([PoPETs 2024](https://petsymposium.org/popets/2024/popets-2024-0151.pdf)), which is exactly what we avoid.
- **Push relay:**
  - APNs and FCM credentials belong to whoever publishes the app in the stores, so a self-hosted server can't send pushes to our store app by itself.
  - Plan: **we run a tiny push relay** (part of the same repo, `axiond-push`). Each axiond sends it `{device_token, opaque_id}` and nothing else.
  - People who build their own app can point at their own relay.
  - On Android, **UnifiedPush** is an option for de-Googled phones.
  - Notification text settings (name and content / name only / nothing) live on the device. We also tell users that iOS keeps notification history, so "nothing" is the safest choice.
- **Mobile client: a shared Rust core with native UI.**
  - The core covers the API client, sync, storage, `dm-crypto` and plugins, exposed through **UniFFI**.
  - The UI is SwiftUI on iOS and Jetpack Compose on Android.
  - Why: push, background fetch, the keychain or keystore, share sheets and accessibility all need native code. iced on mobile isn't ready for a production app.
  - The desktop app keeps iced.
  - This also means moving Darkmatter's non-UI logic into a `darkmatter-core` crate over time, which helps the desktop code too.

## Plugins on the server

- **The same format as client plugins:** `plugin.toml` and newline-delimited JSON, from the shared `dm-plugin` crate. `where = "server"` marks a server plugin.
- **v1:** server plugins are processes the admin installs. They provide slash commands, bots, webhooks and scheduled jobs. They act through a scoped bot token: never an admin token, never another user's.
- **Later:** a WASM sandbox (wasmtime + WIT) with explicit permissions such as network hosts, channels and file access. This is the default once plugins come from third parties.
- **Server plugins can't read E2EE content.** A bot that needs to read an encrypted channel joins as an MLS member, visibly.
- The existing Mattermost-compatible `commands/execute` and `autocomplete_suggestions` endpoints serve plugin commands, so Darkmatter's slash-command UI works unchanged.

## Migration

- **From Mattermost:** import a **bulk export** (`mmctl export create --attachments`).
  - It's JSONL: a `version` line first, then teams, channels, users, posts, replies, reactions, emoji, direct channels and direct posts, plus a folder of attachments ([docs](https://docs.mattermost.com/administration-guide/manage/bulk-export-tool.html)).
  - Mattermost documents bulk export for Entry and the paid plans. **We need to check that it works on our server's edition before we depend on it.** The fallback is a read-only importer that pages through the REST API with an admin token; `mm-api` already speaks it.
- **From Slack:** import the **export zip** (JSON).
  - Free and Pro plans export **public channels only, with file links rather than files**. Free plans keep only 90 days of file links. Private channels and DMs need Business+ approval or Enterprise.
  - The importer downloads linked files while the links still work, so run it soon after exporting.
- **What's imported:** users (as invited, with their old usernames kept), channels, history, threads, reactions, files and custom emoji.
- **Imported history is not end-to-end encrypted.** It arrived in plaintext and the server has seen it. The UI marks it "Imported", and E2EE starts with new messages. Admins can choose to import DMs as "archived, not searchable", or skip them.
- **Moving the team over:** run axiond next to Mattermost, import, check, then switch. Darkmatter can stay logged in to both during the overlap.

## License

**Recommendation: `AGPL-3.0-or-later` for axiond; the shared crates stay `MIT OR Apache-2.0`.**
- **AGPL** means that anyone who runs a modified axiond as a service must publish their changes. Nobody can take it closed or sell a hosted fork without giving back. That's the "more restrictive" you wanted, and it's still real open source: it can go in distros and be self-hosted freely.
- **The shared crates** (`mm-api`, `dm-crypto`, `dm-plugin`) **and Darkmatter stay permissive.** Mobile apps can then ship in app stores without AGPL complications, and anyone can build a compatible client or bot. openmls (MIT) works under both licenses.
- **FSL and BSL** (source-available, turning open after 2–4 years) give more control over commercial use, but they aren't open source. Distros and some contributors avoid them. We'd only need them to stop a competitor selling hosted axiond, and AGPL already covers most of that.
- **CLA or DCO:** if you might ever sell a commercial license (dual licensing) or relicense, you need a CLA from outside contributors, or contributions stay AGPL forever. Otherwise a DCO (`Signed-off-by`) is lighter. **Decide this before the first outside PR.**

## Roadmap

Sizes are rough and assume the current pace: one person plus Claude.

| Milestone | Scope | Size |
|---|---|---|
| **M0: skeleton** (done 2026-10-19) | `crates/axiond` and the shared-crate split. SQLite schema and migrations. Login (argon2id), `users/me`, teams, channels, posts and the websocket with `hello`/`posted`. Darkmatter can log in, read and post. Dockerfile. | 1–2 weeks |
| **M1: Darkmatter feature parity** | All of Appendix A: threads, reactions, files and thumbnails, emoji, search (FTS5), unreads and `view`, set-unread, typing and status, DMs, user autocomplete, slash commands with built-ins, ephemeral messages. TLS and ACME, rate limits, sessions page, TOTP, backups, first-run setup, admin basics (invite, deactivate, roles). Multi-arch image, `axiond-v*` releases. **Friends can use it.** | 4–6 weeks |
| **M2: migration and team switch** | Mattermost and Slack importers, incoming and outgoing webhooks, bot accounts, the audit log, passkeys, Postgres support. **The team moves over.** | 3–4 weeks |
| **M3: E2EE** | `dm-crypto` on openmls, key packages and the delivery service, E2EE DMs and private channels, encrypted files, device list and verification, client search index, recovery key and backup, threat model doc. An external review of the crypto layer before calling it stable. | 6–10 weeks |
| **M4: mobile and push** | `darkmatter-core` extracted with UniFFI, iOS and Android apps (native UI), push relay, UnifiedPush, background sync. | 3–6 months |
| **M5: extensions** | WASM server plugins, calls (WebRTC), sealed sender and padding, maybe federation. | Ongoing |

**M0 as built** (see `crates/axiond/README.md`):
- Darkmatter logs in, reads, posts, replies, edits and deletes. Unreads, mentions, DMs, joining and leaving work, and live events resume after a drop.
- Plus a CLI (`setup`, `user add`, `user password`, `channel add`, `ping`) and a `FROM scratch` Docker image.
- Two changes from the plan:
  - **Queries are checked at runtime, not compile time.** `sqlx::query!` needs a live database or a committed `.sqlx` cache at build time. The integration tests drive every query through Darkmatter's own client (`tests/darkmatter_client.rs`), and that catches the same mistakes.
  - **The shared-crate split is partial.** `mm-api`'s HTTP client is now a default feature, so axiond uses only its models. `dm-crypto` and `dm-plugin` get created when M3 and the server plugins need them.

**Order note:** E2EE (M3) comes after the team switch (M2) so the switch isn't blocked on crypto. The **wire format and API reserve space for E2EE from M0**: `props.dm_e2ee`, device IDs on sessions, and an `encrypted` flag on channels. That way M3 doesn't need a migration of the data model.

## Open questions

1. **E2EE defaults:** is "always on for DMs and private channels, opt-in for public channels" right? Or do you want E2EE everywhere, accepting no server search or server bots in those channels?
2. **History on new devices:** is the optional encrypted server backup with a recovery key OK? The alternative is device-to-device transfer only, which is more private but easier to lose.
3. **Push relay:** are you OK with Karst & Sons running a small public relay for the store apps, given it sees only device tokens and pings?
4. **License:** ~~AGPL-3.0-or-later for axiond?~~ Decided 2026-10-19: AGPL-3.0-or-later. Still open: CLA or DCO? Will you ever want to sell commercial licenses or a hosted version?
5. **Mattermost edition:** which edition and version is the team's server on? That decides whether bulk export works or we use the REST importer.
6. **Slack:** what plan is the workspace on? Do private channels and DMs need to come across?
7. **Scale:** how many users does the largest install need, both the team and the biggest friends' server? That decides when Postgres matters.
8. **SSO:** does the team need OIDC (Google, GitLab, Authentik…) at M2, or are passwords plus passkeys enough?
9. **Calls:** how soon do you need voice and video, and are they part of the Slack replacement?

---

## Appendix A: Mattermost API surface Darkmatter uses today

From `crates/mm-api/src/client.rs` and `ws.rs`. axiond M1 implements exactly this, under `/api/v4/`.

**REST:**

| Area | Endpoints |
|---|---|
| Auth | `POST users/login` (token in the `Token` header), `POST users/logout` |
| Users | `GET users/me`, `POST users/ids`, `GET users/autocomplete?in_team&name&limit`, `GET users/{id}/image`, `POST users/status/ids` |
| Teams | `GET users/me/teams` |
| Channels | `GET users/me/teams/{team}/channels`, `GET users/me/teams/{team}/channels/members`, `GET channels/{id}`, `POST teams/{team}/channels/search`, `POST channels/direct`, `POST channels/{id}/members`, `DELETE channels/{id}/members/{user}`, `POST channels/members/me/view` |
| Posts | `GET channels/{id}/posts?collapsedThreads` (plus paging), `GET posts/{id}/thread`, `POST posts`, `PUT posts/{id}/patch`, `DELETE posts/{id}`, `POST teams/{team}/posts/search`, `POST users/{user}/posts/{post}/set_unread` |
| Threads | `GET users/me/teams/{team}/threads?extended&per_page`, `PUT users/me/teams/{team}/threads/{thread}/read/{ts}` |
| Reactions | `POST reactions`, `DELETE users/{user}/posts/{post}/reactions/{emoji}` |
| Files | `POST files` (multipart), `GET files/{id}`, `GET files/{id}/preview`, `GET files/{id}/thumbnail` |
| Emoji | `GET emoji?page&per_page&sort=name`, `GET emoji/{id}/image` |
| Commands | `POST commands/execute`, `GET teams/{team}/commands/autocomplete_suggestions?user_input&channel_id` |

That's 35 endpoints.

**Websocket** (`/api/v4/websocket`, with resume via `connection_id` and `sequence_number`):
- **Server to client:** `hello`, `posted`, `post_edited`, `post_deleted`, `ephemeral_message`, `typing`, `status_change`, `reaction_added`, `reaction_removed`, `channel_viewed`, `thread_updated`, `thread_follow_changed`.
- **Client to server:** `user_typing`, with `seq` and `seq_reply` acknowledgements.

New in axiond (not Mattermost): `/api/v4/e2ee/*`, websocket events `mls_welcome` and `mls_commit`, `props.dm_e2ee` on posts, and an `encrypted` flag on channels.
