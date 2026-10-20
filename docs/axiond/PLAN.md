# axiond: server plan

axiond is the Darkmatter chat server. This is a plan, not a spec: each section states a decision and why, and the open questions are at the end.

## Who it's for and why

- **Our own group**, moving off Mattermost and Slack. Today we chat on a friend's Mattermost server, through Darkmatter. **The aim is independence from Mattermost:** our own server and our own apps.
  - The group uses **Linux and macOS desktops, iPhones and Android phones.** Nobody uses Windows.
  - axiond will be **run by two of us on a VPS.**
- **Friends and small groups** who want a chat server that's easy to run on almost anything: a home server exposed to the internet, a cheap VPS, a NAS. It's one Docker container with a small footprint.

**Priorities, in order** (decided 2026-10-19):
1. **Encryption and security** of our chats.
2. **Reliability** of both the server and the apps.
3. Features.

Why switch:
- **Mattermost:** the free editions are getting smaller.
  - Mattermost v11 (announced October 2025) introduced **Entry** as the default free edition. Its planned limits include a 10,000-message history cap in the UI and search, 10,000 push notifications a month, and 40-minute 1:1 calls ([Mattermost blog](https://mattermost.com/blog/introducing-mattermost-entry/), [FAQ](https://docs.mattermost.com/product-overview/faq-general.html)).
  - Team Edition keeps full history but has fewer features. It also loses GitLab SSO in v11.
  - We want the whole feature set and the freedom to change it.
- **Slack:** cost and lock-in. Free and Pro exports cover public channels only ([Slack help](https://slack.com/help/articles/201658943-Export-your-workspace-data)).
- **Security and privacy are a top priority.** Conversations shouldn't be easy to intercept or read, even by whoever runs the server.

## Two audiences

Darkmatter and axiond serve two groups of people (decided 2026-10-19):
1. **Darkmatter on its own:** anyone on an existing Mattermost server who wants a fast native client. Darkmatter stays a full Mattermost client and keeps up with Mattermost's releases: its tests run against a pinned Mattermost in CI, and the pin moves with each release.
2. **Darkmatter with axiond:** people who want encryption, security, calls and features they control, on an open-source server with everything included. Darkmatter is at its best here, which is the reason to run both.

What follows from that:
- **Darkmatter asks the server what it supports** and shows features to match. Encryption, devices, verification and our calls appear only on axiond. On plain Mattermost, Darkmatter should feel complete, not cut down.
- **axiond adds to the Mattermost API rather than replacing it.** Core chat stays Mattermost-shaped, so Darkmatter has one code path; extras live under new paths (`/api/v4/e2ee/…`, `/api/v4/calls/…`) and new websocket events.
- **On phones the gap is bigger:** on plain Mattermost our apps get notifications only if that server's admin points push at our relay. That's stated up front.
- **We describe axiond by what it gives** (open source, everything included, encrypted), not by comparison with Mattermost's paid plans.
- **The official Mattermost apps are not supported on axiond** (decided 2026-10-19). They couldn't get push or read encrypted messages.

### One repository or two

The two audiences are a case for splitting into a **client repo** (Darkmatter, MIT OR Apache-2.0) and a **server repo** (axiond, AGPL-3.0-or-later): each gets its own README, issues, releases and license, and Darkmatter-only users never see server work.

The cost is the shared crates (`mm-api`, `dm-crypto`, `dm-plugin`), which both sides use. Split, they'd live in the client repo (they're permissive like it) and axiond would depend on tagged versions. A change to the encrypted wire format then means two coordinated releases instead of one commit, and the end-to-end tests that run Darkmatter's client against axiond have to pin versions across repos.

**Recommendation: split once the E2EE and calls wire formats settle** (after M2 and M3), when the shared crates stop changing every week. Until then one repo keeps those changes atomic and tested together. The split itself is cheap: `git filter-repo` keeps each side's history.

## Goals

1. **It replaces Mattermost and Slack for a team.** That covers:
   - channels, DMs and group DMs;
   - threads, reactions, files, custom emoji;
   - search, mentions and unreads;
   - slash commands, bots and webhooks;
   - presence and typing;
   - **voice and video calls**, end-to-end encrypted, as a core feature (see [Voice and video](#voice-and-video)).
2. **End-to-end encryption by default** wherever it doesn't cost a feature the channel needs (see [E2EE](#end-to-end-encryption)).
3. **Small and simple to run.**
   - One static binary and one Docker image (amd64 and arm64).
   - SQLite by default, so there are no other services to run.
   - Idle RAM under 50 MB.
   - A first-run setup page.
4. **Darkmatter is the client on every platform the group uses:** Linux and macOS now; iOS and Android in M4. Windows isn't planned.
5. **Reliable.** No lost messages, backups that restore, upgrades that don't break, and a server two part-time admins can keep running (see [Reliability](#reliability)).
6. **Migration in:** import from the friend's Mattermost server and a Slack export zip.

## Non-goals (for now)

- **Full Mattermost API compatibility.** We implement the part Darkmatter uses (see [Appendix A](#appendix-a-mattermost-api-surface-darkmatter-uses-today)) so today's Darkmatter runs on axiond from the start. We don't chase the Mattermost web app or the Mattermost mobile apps. The research behind this is in [research/mattermost-rewrite.md](research/mattermost-rewrite.md) (2026-10-19):
  - **The size:** Mattermost has about 600 REST routes, about 108 websocket event types, 88 tables, 310 permissions and about 300k lines of Go, and it adds about 7 routes a month. A drop-in rewrite is roughly 40–70 person-months, then a third to half of a person indefinitely.
  - **Push:** the official phone apps get notifications through Mattermost's push service, which needs a Mattermost license and its terms. They could never get reliable push from axiond.
  - **Plugins:** Mattermost's server plugins speak a Go-only RPC protocol and use Mattermost's own Postgres schema.
  - **Encryption:** the official apps would never speak our E2EE, so their users would always be second-class.
- **Forking Mattermost** (and writing our own versions of its paid features) was also considered and rejected. It would still tie us to Mattermost's code, releases and official apps (the phone app refuses servers that report a version below 9.11, and raises that bar over time). It's Go, not Rust, and it doesn't give us encryption we control.
- **Windows.** Nobody in the group uses it. iced makes it possible later.
- **Federation** between servers. Maybe later (M6+). MLS doesn't rule it out.
- **Huge installs.** The target is 2 to a few hundred users per server. Postgres and multiple nodes are possible later, but they don't drive the design.

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

The detailed design (identities, groups, the delivery service, files, mentions, calls, recovery, what a malicious server can do, and the API) is in [E2EE.md](E2EE.md). This section is the summary.

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

### Review without a paid audit (decided 2026-10-19)

We won't pay for an outside audit. Confidence has to come from the libraries we choose, from community review, and from our own testing. That means:
- **Keep our own crypto code as small as possible.** `dm-crypto` only wires openmls to storage and the wire format. No custom primitives, no custom protocol steps. If something needs crypto that MLS doesn't provide, use a reviewed construction from a reviewed crate (RustCrypto, libcrux) and write down why.
- **Prefer reviewed components.** As of 2026-10-19 we found no published independent audit of openmls itself. NCC Group's public review of XMTP's libxmtp, which is built on openmls, is indirect evidence ([report](https://nccgroup.com/dk/research-blog/public-report-xmtp-mls-implementation-review)). Before M2 starts, check openmls's and mls-rs's current audit status again and record it here. If one has a real audit and the other doesn't, that decides the library.
- **Test hard ourselves:**
  - openmls's RFC 9420 test vectors and interop tests in CI;
  - property tests over group operations (add, remove, update, out-of-order delivery, a device offline for weeks);
  - fuzzing the wire-format parser and everything that reads `props.dm_e2ee` and the `/e2ee` endpoints;
  - tests where a malicious server reorders, replays, drops or forges handshake messages.
- **Publish for review.** The design doc, the threat model and the `dm-crypto` code are written to be read by outsiders. When they're ready, we ask for review where people who know MLS read: openmls's maintainers and community, and security forums.
- **Be honest in the app.** Encryption is labelled **beta** until it has had community review and months of use by our group without problems.

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

### Running it on a VPS, with two admins

- **The hosting provider can read the disk and memory**, so for us the provider is a server operator in the threat model below. E2EE is what protects conversations from it. Disk encryption and SQLCipher on a VPS only protect old disks and snapshots.
- **A deploy kit in the repo:** Docker Compose with axiond serving HTTPS itself (Let's Encrypt built in; calls later need axiond on 443 itself to share it with ICE over TCP), a firewall that opens only 443 (TCP, and UDP for calls) and SSH, SSH by key only, and automatic OS security updates. Written as a short checklist. Caddy in front stays a documented alternative.
- **Two admins, both accountable:** each has their own admin account with 2FA; no shared logins. The audit log moves into M1, so each admin sees what the other did.
- **Backups leave the VPS,** encrypted before they go (for example with restic), so the backup host can't read them. Restoring is tested on a schedule, not just documented.

### Threat model (summary; the full version goes in `docs/axiond/THREAT_MODEL.md`)

| Adversary | Protected? |
|---|---|
| Someone on the network (coffee-shop Wi-Fi, ISP) | Yes. TLS everywhere. |
| A server operator or hosting provider reading conversations | **For E2EE channels, yes:** they get ciphertext and metadata only. For non-E2EE channels, no. That's stated in the UI. |
| A stolen database or backup | E2EE content is safe. Passwords are argon2id hashes. Session tokens are hashed. Non-E2EE content is exposed unless at-rest encryption is on. |
| A compromised server, or a VPS provider, serving malicious keys | Mitigated by device verification and warnings when a key changes. Unverified contacts are a risk, as in Signal. |
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

## Reliability

Reliability is the second priority, for the server and the apps alike:
- **No lost messages.**
  - Darkmatter keeps an outbox: a message sent offline, or while the server restarts, is kept and retried until the server confirms it. `pending_post_id` stops retries from posting twice.
  - Websocket resume with sequence numbers (done in M0) covers short drops; a longer gap falls back to fetching what changed.
- **The database stays consistent.** SQLite in WAL mode, every multi-step change in one transaction, and foreign keys on.
- **Upgrades are safe.** Migrations run inside a transaction, the server takes a backup before applying a new migration, and CI runs every migration over a database made by each earlier release.
- **Backups restore.** CI restores a backup into a fresh server and checks it.
- **Tested the way it's used:**
  - end-to-end tests that drive axiond through Darkmatter's own client (`tests/darkmatter_client.rs`, started in M0);
  - long-running tests that keep several clients posting, reconnecting and restarting the server for hours, and then check nothing was lost or duplicated;
  - Darkmatter's tests also run against a pinned local Mattermost, so either server can't quietly break it.
- **Easy to watch:** `axiond ping` for health checks, clear logs, and a short page for admins on what to do when something goes wrong.

## Voice and video

**A core feature** (decided 2026-10-19): voice and video calls in DMs and channels, with screen sharing, end-to-end encrypted like messages. The research behind this section is in [research/calls.md](research/calls.md) (2026-10-19); everything here is the recommendation from it, to confirm with the Phase 0 spike.

- **How media flows: everything through a small SFU built into axiond**, on **str0m** (Rust, MIT OR Apache-2.0).
  - The SFU forwards each person's audio and video without decoding it. That keeps "one binary, one container".
  - **1:1 calls go through it too:** one code path, and people's IP addresses stay hidden from each other, so no TURN relay is needed to get through NATs.
  - **One UDP port** for media (443/udp suggested), not a range, plus ICE over TCP on 443 for networks that block UDP. TURN over TLS is a later option for networks that let nothing but TLS through.
  - A 2-vCPU VPS should carry one 20-person video call or several small ones (estimated from LiveKit's and Mattermost's benchmarks; Phase 0 measures it). **The real limit is monthly traffic:** a 20-person video call sends about 50 GB an hour, so a 1 TB plan covers about 20 hours a month and a 20 TB plan (Hetzner EU) about 400.
- **Clients use libwebrtc** (Google's WebRTC) through LiveKit's `libwebrtc` crate, with our own signalling (not LiveKit's server). It brings echo cancellation, noise suppression, codecs and congestion control, which would take over a year to rebuild. **Before calls leave beta we build libwebrtc ourselves without H.264/H.265 and FFmpeg**, which the prebuilt archives link (LGPL and patent problems).
- **Encryption:**
  - DTLS-SRTP on every hop, as all WebRTC does.
  - **A fresh MLS group per call** (only the people in the call, with the same device identities as chat, so chat verification carries over). Per-sender frame keys come from its MLS exporter and change on every join and leave.
  - **Phase A:** LiveKit's frame encryption (AES-GCM) keyed from MLS: works on desktop and phones without patching libwebrtc.
  - **Phase B, before calls lose the beta label:** real **SFrame** ([RFC 9605](https://www.rfc-editor.org/rfc/rfc9605)) through a small libwebrtc patch and the `sframe` crate.
  - **Metadata settings:** Opus at a constant bitrate, no DTX and no audio-level header, because variable packet sizes and silence detection give away speech.
- **What the server still sees:** who is in a call, from which device and address, when and for how long, packet sizes and rates, which track is a screen share, and (with Phase A) video resolution and keyframes. Not what anyone says or shows.
- **Screen sharing:** libwebrtc's own capturers (xdg-desktop-portal and PipeWire on Wayland, ScreenCaptureKit on macOS). Video in iced through a shader widget.
- **On phones (M4):** CallKit on iOS and Android's Core-Telecom for the ringing screen. **No PushKit VoIP pushes:** the push relay sends an ordinary push, the notification extension decrypts it and starts the ring itself (Apple's documented path for encrypted calls; it needs the same filtering entitlement as messages).
- **Reliability:** ICE restart on network changes, a two-level reconnect (resume, then rejoin), a call-quality badge, deterministic SFU tests with str0m's network impairment simulator, and headless libwebrtc clients in CI.
- **Escape hatch:** if the Phase 0 spike shows str0m can't keep up, LiveKit's server in a second container with the same client and encryption.

## Mobile and push

**Status (2026-10-19): planned for M4, after encryption and calls work on desktop.** The research behind this section is in [research/mobile-apps.md](research/mobile-apps.md). The phone apps need a demo server, report and block, and account deletion to pass App Store review; those come into M1 and M2 so they don't hold M4 up.

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
- **Things the research found that shape the design:**
  - The iOS notification extension that decrypts a push gets only about 24 MB of memory, so the core needs a lean "decrypt one message" mode. Element X does the same.
  - Hiding a notification on iOS needs an entitlement Apple grants by hand, so every push must produce a visible notification, with "New message" as the fallback.
  - FCM lowers the priority of an app's urgent pushes if they don't lead to a visible notification, so only messages that notify are sent as urgent.
  - Client plugins can't run as subprocesses on phones. The built-in plugins get Rust versions in the core; other plugins become server plugins.
  - `mm-api`'s websocket uses `rustls-native-certs`, which doesn't support Android. It has to move to `rustls-platform-verifier` first.

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
- **A direct importer** is the other fallback: since our friend runs the Mattermost server, it can read about 12 of its Postgres tables (users, teams, channels, members, posts, reactions, files, emoji, preferences, thread memberships, sidebar categories) read-only. Roughly 2–3 weeks.
- **Moving the group over:** run axiond next to Mattermost, import, check, then switch. Darkmatter can stay logged in to both during the overlap. **The switch waits until encrypted chat and calls work on desktop and both phones** (M5).

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
Reordered 2026-10-19 around the priorities (encryption and security, then reliability) and the group's devices. The group moves over only when encrypted chat works on desktop **and** both phones.

| Milestone | Scope | Size |
|---|---|---|
| **M1: a solid foundation** | **First, a short E2EE design** (the wire format, files, search, push, devices and call keys) so everything after is built the encrypted way. Then all of Appendix A: threads, reactions, files and thumbnails, emoji, search (FTS5), set-unread, typing and status, slash commands with built-ins, ephemeral messages. **Security:** TLS and ACME, rate limits, sessions page, TOTP, invites, deactivate, report and block, account deletion, the audit log. **Reliability:** Darkmatter's outbox, tested backups and restores, tested upgrades, long-running tests, Darkmatter's tests against a pinned Mattermost too. **Running it:** the VPS deploy kit, multi-arch image, `axiond-v*` releases. **The two admins run it and use it on desktop.** | 6–8 weeks |
| **M2: E2EE on desktop** | `dm-crypto` on openmls (built for iOS and Android from day one, checked in CI), key packages and the delivery service, E2EE DMs and private channels, encrypted files, device list and verification, client search index, recovery key and backup, the threat model doc, the review steps in [Review without a paid audit](#review-without-a-paid-audit-decided-2026-10-19). Passkeys. Labelled beta. | 2–3 months |
| **M3: calls on desktop** | Phase 0 spike (go/no-go for str0m), then the SFU in axiond, 1:1 and group voice and video, screen sharing, per-call MLS groups with Phase A encryption, Darkmatter's call UI on Linux and macOS (the MVP, about 5.5–6.5 person-months). Then reliability hardening, our own libwebrtc build and SFrame (Phase B). | about 8.5–10.5 person-months |
| **M4: phones** | `darkmatter-core` extracted (started earlier where it helps desktop), UniFFI, Android and iOS apps (native UI) with E2EE and calls (calls add about 6.5–8 person-months), the push relay and encrypted push, UnifiedPush, App Store and Play setup with a demo server. | about 10–14 person-months |
| **M5: the group moves over** | The Mattermost importer (bulk export, or reading Postgres directly), Slack if still needed, webhooks and bot accounts, then the switch. | 3–4 weeks |
| **M6: extensions** | WASM server plugins, sealed sender and padding, maybe federation, Postgres. | Ongoing |

At this pace the switch is roughly two and a half years away (M1–M4 is about 30 person-months). The quickest ways to shorten it: ship encrypted **voice** calls first (about 2.5–3 person-months on desktop) and video later, and give the group the M3 MVP while hardening continues. Until then the group keeps using Darkmatter on the friend's Mattermost server. If that's too long, the lever is M4: the same Rust core under Flutter, rather than two native apps, saves roughly 30–40% of the UI work.

**M0 as built** (see `crates/axiond/README.md`):
- Darkmatter logs in, reads, posts, replies, edits and deletes. Unreads, mentions, DMs, joining and leaving work, and live events resume after a drop.
- Plus a CLI (`setup`, `user add`, `user password`, `channel add`, `ping`) and a `FROM scratch` Docker image.
- Two changes from the plan:
  - **Queries are checked at runtime, not compile time.** `sqlx::query!` needs a live database or a committed `.sqlx` cache at build time. The integration tests drive every query through Darkmatter's own client (`tests/darkmatter_client.rs`), and that catches the same mistakes.
  - **The shared-crate split is partial.** `mm-api`'s HTTP client is now a default feature, so axiond uses only its models. `dm-crypto` and `dm-plugin` get created when M2 (E2EE) and the server plugins need them.

**M1 as built** (2026-10-20):
- **Done:**
  - The E2EE design ([E2EE.md](E2EE.md)), so files, search, mentions, reactions and devices are built the way encryption needs.
  - All of Appendix A, plus custom emoji, profile pictures and statuses set by hand.
  - Security: HTTPS (Let's Encrypt or your own files) with HSTS, rate limits, signed-in apps, two-factor codes, invite-only sign-up, deactivating, report and block, deleting accounts, the audit log, hourly housekeeping that deletes old deleted files.
  - Reliability: Darkmatter's outbox (no lost messages, no doubles: posts are idempotent by `pending_post_id`), backups and restore, a pre-upgrade copy of the database, upgrade tests from older databases, a crash soak test (SIGKILL every few seconds, nothing lost or doubled), and Darkmatter's client tested against Mattermost 11.7.12 and 12.0.0 in CI.
  - Running it: the VPS deploy kit (`deploy/axiond`) and a release workflow for multi-arch images.
  - Darkmatter: invite sign-up, the Account window (signed-in apps, two-factor codes with a QR code, blocked people, deleting the account), ⚑ report or block, and telling axiond from plain Mattermost.
- **Changes from the plan:**
  - **The outbox is a JSON file** (state dir, mode 0600), not the SQLCipher database. That database arrives in M2 with the MLS state that needs it; the outbox moves into it then.
- **Soak result:** 10 minutes, 249 SIGKILL crashes, 13,822 messages from five people: none lost, none doubled, all in order. **To look into:** six "database is locked" errors in one five-second burst (SQLite busy, codes 5 and 517). Clients retried and nothing was lost, but a server under normal load shouldn't return them.
- **Not checked yet:** Let's Encrypt against a real domain, the release workflow's first run (arm64 under emulation), and the outbox in the running app (only through tests). The two of you running it on a VPS is the real test of M1.

**Order note:** the **wire format and API reserve space for E2EE from M0**: `props.dm_e2ee`, device IDs on sessions, and an `encrypted` flag on channels. That way M2 doesn't need a migration of the data model.

## Open questions

1. **E2EE defaults:** is "always on for DMs and private channels, opt-in for public channels" right? Or do you want E2EE everywhere, accepting no server search or server bots in those channels?
2. **History on new devices:** is the optional encrypted server backup with a recovery key OK? The alternative is device-to-device transfer only, which is more private but easier to lose.
3. **Push relay:** are you OK with Karst & Sons running a small public relay for the store apps, given it sees only device tokens and pings?
4. **License:** ~~AGPL-3.0-or-later for axiond?~~ Decided 2026-10-19: AGPL-3.0-or-later. Still open: CLA or DCO? Will you ever want to sell commercial licenses or a hosted version?
5. **Mattermost edition:** which edition and version is the friend's server on? That decides whether bulk export works, or we read its database directly. (Needed by M5.)
6. **Slack:** is Slack still in use? If so, what plan, and do private channels and DMs need to come across?
7. **Scale:** how many people are in the group? That decides when Postgres matters.
8. **SSO:** does anyone need OIDC (Google, GitLab, Authentik…), or are passwords plus passkeys enough?
9. **Calls:** decided 2026-10-19 that calls are a core feature; the research answered the library choices. Still open: the largest group that needs video (20 is feasible on 2 vCPU), and whether encrypted voice calls should ship before video.
10. **Which phone first** in M4, Android or iPhone?
11. **Encryption library:** openmls or mls-rs, decided by their audit status at the start of M2.
12. **Review without a paid audit (decided):** no paid audit; we rely on reviewed libraries, our own testing and community review.

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
