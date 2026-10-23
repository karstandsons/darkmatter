# Could axiond become a full Mattermost server? (RST-1)

> **Since then (2026-10-23):** there's no import from Mattermost. Moving to axiond is a fresh start, so the importers below (§2.6) aren't planned, and Slack is no longer in use. See [PLAN.md](../PLAN.md#starting-fresh-on-axiond).

**Short answer: no. Keep the non-goal.** A drop-in Rust replacement for Mattermost works out to about **40–70 person-months** (one developer working with Claude), or 120–200 person-months at a traditional pace. After that it needs **0.3–0.5 of a person indefinitely** just to keep up, because Mattermost adds about 7 endpoints a month. It also works against the plan's main goal: the official apps can never take part in E2EE.

Two cheaper options make sense:
- **(c) Stay Darkmatter-only** for now.
- **(b-web)** Later, if people without Darkmatter (browser or Windows users) need access, add a narrow tier: **the official web app, pinned to one version, on plaintext channels only.** About 2–4 person-months.

Do **not** target the official mobile apps. They can't get push notifications from a non-Mattermost server in production (§2.4).

Details, numbers and how I got them are below.

---

## 0. Method and sources

- **What I read.** Shallow clones, taken 2026-10-19, of:
  - `github.com/mattermost/mattermost` at `9df9842`, which is 12.1.0-dev (the latest tag is v12.0.0, 2026-10-14);
  - `mattermost/mattermost-mobile`;
  - `mattermost/mattermost-push-proxy`.

  I also used a blobless clone of the full history to compare releases.
- **How I counted.** Every number below comes from grep, awk or `wc` over those sources. File paths are relative to each repository.
- **Who traced what.** Two read-only subagents traced the web-app and mobile-app startup code. I spot-checked their key citations.
- **axiond itself** is read from `main` (commit `1f8aa44`), because this task's branch predates it.
- **What I did not touch.** No server was contacted. Nothing was run against the team's current Mattermost server. Nothing in the repo was changed.
- **The baseline: axiond today** (`main`):
  - 27 routes in `crates/axiond/src/api/mod.rs`;
  - 7 tables;
  - 2,519 lines of Rust and SQL.

## 1. How big the target is

### 1.1 REST endpoints

| Measure | Count | How |
|---|---|---|
| Registered public routes | **603** | `.Handle(` calls in `server/channels/api4/*.go`, excluding tests and `*_local.go` |
| OpenAPI operations | **604** | get/put/post/delete/patch under `/api/v4` in `api/v4/source/*.yaml` (59 files). This agrees with the route count |
| Local-mode routes (unix socket, used by `mmctl --local`) | **171** | `.Handle(` in `api4/*_local.go`. Mostly the same handlers re-mounted |
| Go client methods (`model.Client4`, used by mmctl, bots and integrations) | 798 | `^func (c *Client4)` in `server/public/model/client4.go` |
| Web app client methods | ~565 | Arrow methods in `webapp/platform/client/src/client4.ts` (5,593 lines) |
| Mobile app client endpoints | ~204, plus 56 more for Playbooks, Calls and Agents | `doFetch` call sites in `mobile/app/client/rest/*.ts` |

**Routes by area** (route counts per `api4/*.go` file; the matching OpenAPI file is in brackets where it differs):

| Area | Routes | Notes |
|---|---|---|
| Users | 78 | Includes sessions, MFA, tokens, 7 thread routes, login variants and SSO code exchange |
| Channels | 62, plus 7 join-request, 5 bookmarks, 7 channel views | |
| Teams | 37 | |
| Posts | 27, plus 4 drafts, 4 scheduled posts | |
| Files / uploads | 9 + 3 | |
| Preferences | 5 | |
| Threads (collapsed reply threads, "CRT") | ~10 | Spread across user.go and post.go |
| Search (posts, users, channels, teams, files, groups) | ~21 | Routes with "search" in the path |
| Emoji | 9 | |
| Reactions | 3 | Some reaction paths are registered in post.go |
| Status | 7 | |
| Roles / schemes / permissions | 5 / 7 / 1 | Small route count, but the logic is large (§2.1) |
| Groups (LDAP and custom) | 20 | |
| System / config / license | 31 / 6 / 7 | 51 operations in `system.yaml` |
| Plugins | 12 | Plus `/plugins/{id}/*`, which is proxied to each plugin |
| Webhooks / slash commands / bots / interactive actions | 11 / 10 / 8 / 5 | |
| OAuth provider / outgoing OAuth connections | 9 / 6 | |
| SAML / LDAP | 11 / 13 | Their implementation is enterprise code (§2.5) |
| Compliance / data retention / exports / imports / reports | 4 / 17 / 4 / 2 / 4 | |
| Elasticsearch / OpenSearch | 2 | **Bleve is gone**: only constant names remain in `model/permission.go` and `audit_events.go` |
| Cloud / hosted customer / usage / limits | 14 / 1 / 3 / 1 | |
| Remote clusters / shared channels (federation) | 12 / 7 | |
| Access control (ABAC) / custom profile attributes / properties | 17 / 8 / 13 | |
| Content flagging / recaps / scheduled recaps / agents (AI) | 14 / 8 / 7 / 3 | |
| Jobs / logs / metrics / health / cluster / IP filters / brand / ToS / audit logging / delivery tracking / ephemeral mode | 7 / 1 / 1 / 3 / 1 / 3 / 3 / 2 / 2 / 2 / 3 | |
| Boards / Playbooks / Calls | 1 route in core | Everything else is in **plugins** (§2.2): Playbooks has about 25 endpoints the mobile app uses, Calls about 17 |

### 1.2 Websocket

- **Event types (server to client): ≈108–110.** 108 `WebsocketEventType = "…"` constants at HEAD in `server/public/model/websocket_message.go`; a looser count over the tags in §2.8 gives 110 for v12.0.0.
- The web app's `handleEvent` switch has 96 cases (`webapp/channels/src/actions/websocket_actions.ts`). The mobile app has about 113 constants (`mobile/app/constants/websocket.ts`).
- **Actions (client to server): 8.**
  - 6 are registered in `server/channels/wsapi/*.go`: `user_typing`, `ping`, `get_statuses`, `get_statuses_by_ids`, `user_update_active_status`, `posted_notify_ack`.
  - The other 2 are `authentication_challenge` (used by mobile) and `presence` (used by the web app).
- **Resume protocol:** `connection_id` and `sequence_number` (axiond already has this), plus `posted_ack=true`.

### 1.3 Everything else that makes up "the server"

| Thing | Size | How |
|---|---|---|
| Server Go, excluding tests, mocks and generated code | 468,841 lines | `find … -name '*.go'`, excluding `_test`, mocks, `*generated*` and `_gen` |
| Hand-written production Go a rewrite would actually port | **≈300–320k** | The line above, minus the timer and retry store layers (36.7k, generated), `storetest`/`searchtest` (81.8k of test helpers), mmctl (15.7k) and `enterprise/` (14.1k, source-available) |
| Largest pieces | `app` 110.9k; `sqlstore` 41.4k; `api4` 39.1k; `model` 58.1k; plugin host (`app/plugin*.go`) 25.9k; `platform` 18.8k | |
| Go tests | 439,536 lines | These run in-process against Go internals, so axiond **can't reuse them** |
| Database | **88 tables**, 235 Postgres migrations | `CREATE TABLE` in `server/channels/db/migrations/postgres/*.up.sql`. **MySQL support was removed** on 2025-07-22 (`MM-63368`) |
| Config | **45** `*Settings` sections, **≈585** fields | `server/public/model/config.go`, 6,104 lines |
| Client config sent to apps | ≈290 keys | `props["…"]` in `server/config/client.go` |
| Permissions | **310** | `Permission… = &Permission{` in `model/permission.go` |
| Built-in roles | **24** | `*RoleId` constants in `model/role.go` |
| Plugin API | **263** methods (API) and **53** hooks | `public/plugin/api.go`, `hooks.go` |
| License | 30 boolean feature flags | `model/license.go`. License checks appear in 75 non-test files under api4/app |
| Server i18n strings | 3,884 | `server/i18n/en.json` |
| Web app / mobile TypeScript, for scale | 447k / 224k lines | |

### 1.4 What the official apps need just to load and chat

**Web app: about 47 endpoints plus the websocket.** Full trace by a subagent from `webapp/channels/src`.

- **Auth is cookie-based**, not a header. It uses `MMAUTHTOKEN`, `MMUSERID`, and an `MMCSRF` cookie echoed back as `X-CSRF-Token`; see `client4.ts:607,627` and `utils/utils.tsx:1643`. axiond uses the `Token` header today.
- **Most failures only degrade the app.** Redux actions catch errors (`mattermost-redux/src/actions/helpers.ts:94-100`), so a 501 usually removes a feature rather than blanking the page.
- **Failures that break the app:**

| Failure | Effect |
|---|---|
| `GET /users/me` fails | **Login loop**: it logs out and returns to `/login` (`logged_in.tsx:103-108`) |
| `/users/me/teams` or `/teams/members` fails | Redirects to `/error?type=team_not_found` |
| `POST /roles/names` fails, or the server version is missing (no `X-Version-Id` header and no `server_version` in `hello`) | Roles never load (`roles.ts:85-89`), so `canPost` is false and **the message box is read-only** |
| `GET /users/{id}/teams/{tid}/channels/categories` fails | The sidebar is empty |
| No `WebsocketPort` / `WebsocketSecurePort` in the client config | The websocket URL is broken (`websocket_actions.ts:228-233`) |
| Error bodies aren't Mattermost JSON with `id` and `status_code` | A failed login can be **treated as a success** (`client4.ts:4974-4981`) |

- **Calls it tolerates failing:** `/limits/server`, `/plugins/webapp`, drafts, scheduled posts, groups, custom profile attributes, apps bindings.
- **The minimum set (47), by group:**

| Group | Count |
|---|---|
| Config, license, login, logout | 4 |
| `users/me`, preferences (get and put), teams, team members, team unreads | 6 |
| `roles/names` | 1 |
| Channels | 10 (incl. `users/me/channels`, `users/{id}/channel_members`, categories, by-name, stats, join, view) |
| Posts | 4 (incl. `posts/unread`) |
| Reactions | 2 |
| Files | 4 |
| Users | 5 (`ids`, `usernames`, `image`, `autocomplete`, `group_channels`) |
| Status | 1 |
| DM and group DM creation | 2 |
| Emoji | 4 |
| Search | 1 |
| CRT threads | 3 |

- **What else must be on the wire:**
  - roughly 20 websocket event types;
  - the client-to-server actions `presence` and `ping`/`seq_reply`;
  - the server must **serve the web app bundle itself** (static files at `/`).
- **Version checks** in the web app are trivial: a version must be present, and `isMinimumServerVersion` is checked against 5.10 and 5.14.

**Desktop app.** It is an Electron shell that loads the web app from the server, so it needs what the web app needs. I did not trace it separately.

**Mobile app: about 22 endpoints plus the websocket.** Traced from `mobile/app`.

- **Version checks** (`app/constants/supported_server.ts:4-8`):
  - `MIN_REQUIRED_VERSION = '9.11.0'`: below it, everyone gets an upgrade alert that can't be dismissed.
  - `FULL_VERSION = '11.7.0'`: below it, admins get an alert.
  - Both are read from the `X-Version-Id` header.
- **Adding a server.** It calls `system/ping`, then `config/client` and `license/client` (`format=old`). `DiagnosticId` must be present, because it is used as the server's identity (`screens/server/index.tsx:384`).
- **Login:** `POST users/login`, then `PUT users/sessions/device`.
- **Entry sync, which runs on every websocket (re)connect** (`actions/remote/entry/common.ts:85-200`):
  - Fatal if these fail: config, license, `users/me/preferences`, `users/me/teams`, `users/me/teams/members`, `users/me`.
  - Degraded if these fail: channels, channel members, categories, `roles/names`.
- **Chat:**
  - `channels/{id}/posts`, `posts/{id}/thread`, `users/{u}/teams/{t}/threads`;
  - `POST posts`, `channels/members/me/view`, reactions, files;
  - `users/ids`, `users/status/ids`.
- **Push acknowledgements:** `POST notifications/ack`.
- **Websocket:** authenticates with `authentication_challenge`. It handles `hello`, `posted`, `post_edited`, `post_deleted`, `reaction_*`, `channel_viewed`, `multiple_channels_viewed`, `typing`, `status_change`, `preferences_changed`, `user_added`, `user_updated`, `channel_created`, `thread_updated` (`actions/websocket/event.ts:46-186`).
- **Plugins are probed but optional:** Playbooks, Calls, Agents, Apps and ABAC only degrade if missing.

**Combined.** The web and mobile sets together come to about **55–60 distinct endpoints**, about 20 websocket events, cookie auth with CSRF, Mattermost-format errors, and a version string of at least 11.7.0. That is under **10% of the API**. axiond's Appendix A (35 endpoints) already covers about half of it.

The catch is **shape fidelity**:
- every field the apps read in User, Channel, ChannelMember, Post, Preference, Category, Role and ClientConfig must be there;
- websocket `data` must match too. For example, `posted` carries the post as a JSON *string*.

## 2. The hard parts

### 2.1 Permissions, roles and schemes

- **The size:** 310 permissions and 24 built-in roles.
- **How effective permissions are computed.** They come from system roles, plus team roles from the team's **scheme**, plus channel roles from the channel's scheme. On top of that sit "higher-scoped" roles, channel moderation (which edits the scheme roles behind the scenes), guest roles, and bot or integration special cases.
- **Where the logic lives:**
  - `app/authorization.go` (917 lines), `role.go` (382), `scheme.go` (232), `permissions.go` (152);
  - the role and scheme stores (about 1,000 lines);
  - 69 functions of permission migrations (`app/permissions_migrations.go`). These have changed the default role contents release after release.
- **Clients don't just obey 403s.** They download roles (`POST /roles/names`) and **compute permissions themselves** to decide which UI to show. So an exact port means matching every default role's permission list, version by version.
- **The cheap answer for a compatibility tier:** a fixed set of roles (`system_user`, `team_user`, `channel_user`, the admin roles) whose permission lists are copied once. The server still enforces axiond's own simpler model.

### 2.2 Server plugins (Go) and web app plugins (JS)

- **How Go plugins run.** Each server plugin is a separate Go binary launched with **hashicorp/go-plugin v1.8.0**. The handshake uses the cookie `MATTERMOST_PLUGIN` = "Securely message teams, anywhere.", protocol 1 (`public/plugin/api.go:1851`).
- **The protocol is Go-only.** Plugins speak Go **`net/rpc` with `encoding/gob`** over yamux streams, not gRPC. Calls go both ways: 263 API methods and 53 hooks, with 9,813 generated lines in `client_rpc_generated.go`. Go-specific types are gob-registered by name (`client_rpc.go:225-240`). There is also `ServeHTTP` hijacking (`http.go`, `hijack.go`).
- **Could a Rust server host them?** Technically yes. It would need:
  - a gob encoder and decoder, compatible with Go's reflection-based type names;
  - net/rpc framing;
  - yamux;
  - all 263 API methods with Mattermost semantics.

  That is a large, fragile, Go-specific project with no Rust library to lean on.
- **Plugins also get direct database access.** `public/plugin/db_rpc.go` proxies a `database/sql` driver over RPC. Playbooks and Boards keep their own tables in **Mattermost's Postgres** and join against `Users`, `Channels` and so on. Hosting them means giving them Postgres with Mattermost's schema, which rules out axiond's SQLite-first design for those plugins.
- **Prepackaged plugins** (`server/Makefile:161-170`): Calls, GitHub, GitLab, Jira, Playbooks, Zoom, Agents, **Boards**, User Survey. Calls also needs a WebRTC SFU (built in, or `rtcd`).
- **Web app plugins are easy by comparison.** They are JS bundles: the server lists them at `GET /plugins/webapp` and serves the static files, and the web app loads them. Most of them, though, call their own server half under `/plugins/{id}/…`, so a web-only plugin rarely works on its own.

**Verdict.** "Mattermost plugins work unchanged" is the single most expensive line item: about 8–15 person-months, on top of an exact port of the 263 API methods. It also forces Postgres with the Mattermost schema. For a tier, answer `PluginsEnabled=false`; the apps handle that.

### 2.3 Search

- **The default** is SQL full-text in `sqlstore` (Postgres `tsvector`).
- **The query syntax** has its own parser in `model/search_params.go` (398 lines): `from:`, `in:`, `on:`, `before:`, `after:`, `-excludes`, quoted phrases, hashtags, and wildcards. File search is separate.
- **Elasticsearch and OpenSearch** live in `server/enterprise/elasticsearch` under the source-available license.
- **Bleve has been removed.**
- Porting the syntax onto FTS5 or `tsvector` is moderate work, about 1 person-month. The plan's FTS5 approach is fine.

### 2.4 Push notifications

This is a dead end for the official mobile apps.

- **How the server sends.** It POSTs to `{PushNotificationServer}/api/v1/send_push` (`app/notification_push.go:547-585`). The payload is defined in `model/push_notification.go:86-115`.
- **Device registration.** Devices register as `apple_rn-v2:<token>` or `android_rn-v2:<token>`, through `PUT users/sessions/device`.
- **The push proxy** is open source (Apache-2.0, 2,064 lines of Go). But **only Mattermost holds the APNs key and FCM service account** for the store apps' bundle IDs.
- **The hosted service (HPNS: `push.mattermost.com` and regional hosts)** has two gates:
  - the Mattermost server requires the license feature **`MHPNS`** (`app/notification.go:33`, `push_notification_server.go:46-60`);
  - it comes with the HPNS Terms of Service.
- **The test service (TPNS: `push-test.mattermost.com`)** is "no SLA, limited throughput, not for production."
- **Consequences:**
  - A non-Mattermost server sending to HPNS would at least break its terms. Whether HPNS itself checks a license isn't visible in the open source.
  - So **the official store apps can't get reliable push from axiond.** Without push, a chat app on a phone is mostly useless.
  - The only real fix is to publish your own build of the mobile app (Apache-2.0) with your own push proxy. That is the same store and publishing work as M4.
- **Privacy:** the payload includes `message`, `sender_name` and `channel_name` unless `id_loaded` is used, and `id_loaded` is itself gated by a license check (`notification_push.go:806-811`).

### 2.5 License-gated and enterprise features

- **The licensing.** The server is AGPL-3.0. `server/public/` and `webapp/` are Apache-2.0. `server/enterprise/` is **source-available**, usable only with an E20 subscription (`server/enterprise/LICENSE`). Interfaces for the closed parts are in `server/einterfaces/`: LDAP, SAML, compliance, data retention, cluster, message export, IP filtering, access control, Intune, push proxy auth, and more.
- **What that means for a rewrite.** SAML, LDAP, Elasticsearch, compliance export, data retention and clustering would have to be written **clean-room**, because axiond's authors can't port from the source-available code.
- **License flags drive the apps' UI.** The apps read `license/client` (30 feature flags, plus `IsLicensed`) to switch UI on. Returning a fake "licensed" JSON would unlock enterprise UI in the Apache-licensed web app. That is legal under the code license, but hostile to Mattermost and a trademark and goodwill risk. **Don't do it.**
- **Porting core logic is fine.** axiond is AGPL too, so porting from the AGPL core is license-compatible.
- **The name can't be used.** "Mattermost" can't appear in axiond's name (see `TRADEMARKS.md`, which already says this).

### 2.6 Database schema and migrating existing Mattermost data

The schema is **88 tables and 235 migrations**, and about 91 new migrations landed in the last 12 months. There are two ways to approach it.

**(i) Adopt Mattermost's Postgres schema as-is.**
- You get zero-step migration, plugins can use it, and the team can switch back.
- But axiond would have to own **every future Mattermost migration**, which gives up SQLite-first and the data model reserved for E2EE.

**(ii) Keep axiond's own schema and import.** Two importers are possible:
- **Bulk export** (JSONL, already in the plan).
- **A direct read-only Postgres importer** reading about 12 tables: `Users`, `Teams`, `TeamMembers`, `Channels`, `ChannelMembers`, `Posts`, `Reactions`, `FileInfo`, `Emoji`, `Preferences`, `ThreadMemberships`, `SidebarCategories`. This sidesteps the question of which edition bulk export works on (plan, open question 5), and costs about 2–3 weeks.

**Recommendation: (ii).**

### 2.7 Config surface

- Mattermost has 45 settings sections with about 585 fields. The apps receive about 290 client-config keys, plus `FeatureFlag*` keys, where a missing flag reads as "off".
- **A tier needs about 40–60 keys:**
  - login methods;
  - websocket ports;
  - `CollapsedThreads`;
  - `EnableUserStatuses` and `EnableUserTypingMessages`;
  - `MaxFileSize` and `MaxPostSize`;
  - `EnableCustomEmoji`;
  - `TeammateNameDisplay`;
  - `PluginsEnabled=false`;
  - `DiagnosticId`;
  - `Version`/`BuildNumber`.
- **A full rewrite** would need the System Console (about 90 admin routes) to round-trip the whole `config.go` model.

### 2.8 Version drift

Measured over release tags (blobless clone; routes counted the same way as §1.1):

| Tag | Date | api4 routes | Websocket events |
|---|---|---|---|
| v9.0.0 | 2023-09 | 454 | 74 |
| v10.0.0 | 2024-09 | 468 | 78 |
| v10.11.0 | 2025-07 | 508 | 87 |
| v11.3.0 | 2026-01 | 534 | 91 |
| v11.6.0 | 2026-04 | 541 | 93 |
| v11.9.0 | 2026-07 | 577 | 108 |
| v12.0.0 | 2026-10 | 600 | 110 |

- **Routes:** v10.11 to v12.0 (15 months) added **104 routes** and removed or rewired 12. That is about 7 a month, and the pace is speeding up: +146 routes in 3 years, +59 in the 6 months from v11.6 to v12.0.
- **Commits in the last 12 months:** 321 touched `api4`, 393 touched `model`, 2,122 in total.
- **Releases** ship monthly (v11.1 through v11.11, then v12.0, roughly 4 weeks apart).
- **Clients raise their minimums too.** The mobile app now alerts below 11.7.0, so a compatible server must keep *claiming* newer versions, and clients enable features based on that claim.
- **What this means for each option:**
  - A full clone must chase this forever.
  - A web-only tier can **pin** the web app bundle it serves. Desktop loads that pinned bundle too, so drift only bites when the pin is deliberately moved.
  - Mobile can't be pinned, because users auto-update from the stores.

## 3. How E2EE fits

- **The official clients will never speak MLS.** In a room that mixes Darkmatter users and official-app users there are only bad choices:
  1. **Downgrade the room to plaintext** when an official-app user is a member. That defeats "E2EE always for DMs", and it gives a malicious operator a **downgrade lever**: the operator can claim a member is on a legacy client.
  2. **Keep the room encrypted, and official clients see the placeholder** ("🔒 Encrypted message", as the plan's `props.dm_e2ee` design already does). This is safe, but official-app users can't read or meaningfully reply. axiond should *reject* plaintext posts from legacy clients into E2EE channels, or the room silently mixes encrypted and plaintext messages.
  3. **Decrypt on the server for legacy clients.** This isn't E2EE. Ruled out.
- **Encryption in the official web app is also a dead end.** Even if someone added MLS to the official web app, its code is **served by the server**, so a malicious operator could serve a backdoored bundle. That's the known weakness of web-delivered E2EE, and Darkmatter as a native, signed app avoids it.
- **So the official apps can only ever be second-class.** The workable rules:
  - official apps are fine for plaintext public channels;
  - E2EE channels show them a locked placeholder;
  - the channel's E2EE flag is sticky and never downgrades automatically.
- That works, but a "Mattermost-compatible" axiond would mostly be a plaintext Mattermost. That's the product the team is leaving.

## 4. Strategy options

The person-month (PM) figures assume one experienced developer working with Claude at the pace the plan's roadmap assumes. The "traditional" column is the same work without that assistance.

**How I estimated:**
- **By lines of code:** about 300–320k lines of hand-written production Go to port. At 1.5–2.5k delivered and tested lines per person-month, that's **120–210 PM**.
- **By route:** 603 routes at 0.2–0.3 PM each comes to **120–180 PM**.
- **With Claude:** about one third of that, giving **40–70 PM**. Conformance debugging doesn't shrink as much as code writing does.
- **Sanity check against the plan:** M1 is 4–6 weeks for 35 endpoints plus infrastructure.

| Option | What you get | Effort, with Claude | Traditional | Ongoing cost | Risk |
|---|---|---|---|---|---|
| **(a) Full rewrite** | Official web, desktop and mobile apps, mmctl, bots, integrations and plugins unchanged | **40–70 PM**, breaking down as: core chat ~15–20; permissions and schemes ~3–5; integrations and OAuth ~4–6; admin, config and jobs ~6–10; enterprise features rewritten clean-room ~8–12; Go plugin host with gob/net/rpc and DB proxy ~8–15; push and email ~1–2 | 120–200 PM | 0.3–0.5 FTE indefinitely (~7 new routes a month, monthly releases) | **Very high.** Never finished. Forces Postgres with Mattermost's schema for plugins. Official mobile apps still have no production push (§2.4). E2EE stays second-class (§3) |
| **(b) Compatibility tier: web and mobile can load and chat** | The official web app (served and pinned by axiond) and desktop work for channels, DMs, threads, reactions, files, emoji, search, unreads, typing and presence. Mobile works too, but **without reliable push** | **3–5 PM** on top of M1. Web-only **(b-web)** is **2–4 PM**: ~25–30 new endpoints, cookie auth and CSRF, the error format, `X-Version-Id`, fixed roles, categories, preferences, ~20 websocket events, serving the bundle, and the conformance harness | 8–15 PM | Low if web-only and pinned. Medium with mobile, because store updates move its required calls | Medium. Shape fidelity, with official clients showing placeholders in E2EE rooms. Mobile without push is a poor experience |
| **(c) Stay Darkmatter-only** (current plan) | Darkmatter on all platforms; full E2EE | 0 extra. M4's native mobile apps (3–6 months) remain the cost | — | None from Mattermost | Low. Needs Darkmatter on Windows and mobile before every friend can join |
| **(d) Run real Mattermost (Team Edition) and add our features around it** | Every official app, every plugin, mmctl. E2EE for Darkmatter users through an MM plugin: an MLS delivery service using plugin KV storage, `/plugins/{id}/…` endpoints and `PublishWebSocketEvent`. The plan's `props.dm_e2ee` already works on stock Mattermost | 1–2 PM for the plugin, plus the same M3 client work | 3–5 PM | Follow Mattermost upgrades; the plugin API is stable | Medium. Gives up "small, Rust, SQLite, <50 MB" (Postgres only since MySQL's removal; a much bigger footprint). Exposed to further free-edition cuts (Entry limits, GitLab SSO dropped in v11). Plugin in Go. Official apps still can't decrypt |

**What each option costs on the plan's goals:**
- **(a)** sacrifices goal 3 (small and simple) and blurs goal 2 (E2EE by default) without fully achieving goal 1 on mobile.
- **(d)** is the honest "Mattermost compatibility" option. It's cheaper than (a) by an order of magnitude, but it abandons axiond.
- **(b-web)** is the only compatibility work that pays for itself. It gives browser access to people without Darkmatter (Windows, a borrowed laptop, guests) at a cost similar to one roadmap milestone.

### 4.1 Conformance testing

This is needed for (b), and essential for (a).

1. **Record.**
   - Start `mm-dev` from `dev/seed.sh` (the `mattermost/mattermost-preview` image), **pinned to a tag** rather than `latest`.
   - Drive the official web app with Playwright through scripted journeys: login, sidebar, open channel, scroll back, post, edit, delete, thread reply, CRT list, react, upload, search, DM and group DM creation, mark unread, typing, reconnect.
   - Capture a HAR file plus websocket frames (`page.on('websocket')`). Save cookies and headers.
   - For mobile: run the same journeys in an Android emulator behind mitmproxy, or replay the REST sequence from `mobile/app/actions/remote/entry/*`.
2. **Normalize.**
   - Map IDs to placeholders in order of first appearance; replace timestamps, tokens and ETags.
   - Sort arrays where order isn't meaningful.
3. **Seed axiond identically.** Either axiond implements the admin calls `seed.sh` uses (`POST /users`, `/teams`, `/channels`, members), or an `axiond seed` command builds the same fixture.
4. **Replay and diff.**
   - Replay each request against axiond and compare status codes, the key headers (`X-Version-Id`, `Set-Cookie` with the `MM*` cookies, `Content-Type`), and JSON structure.
   - Only compare the fields the clients **read**, using an allowlist built from the TypeScript types in `webapp/platform/types` and `mobile/app/database/models`. Extra fields are fine; missing fields fail.
   - Compare websocket event sequences by type, broadcast scope and `data` keys.
5. **Smoke tests.**
   - Run the official Playwright e2e suite (658 spec files in `e2e-tests/playwright`, `MM_SERVICESETTINGS_SITEURL` pointed at axiond) **filtered to the chat specs**. Most specs set up through admin APIs, so expect a small passing subset at first. That's still useful as a smoke test.
   - Validate response shapes against `api/v4/source` with an OpenAPI fuzzer such as schemathesis, limited to the implemented operations.
6. **Track drift.** On each Mattermost release, re-record and diff the route table between tags, as in §2.8. This would have shown "+104 routes, 12 removed or rewired" for v10.11 to v12.0. Run it in CI against a local mm-dev only, **never against the team's current Mattermost server**.

## 5. Recommendation

1. **Keep the plan's non-goal.** Don't build a full Mattermost reimplementation. The numbers are:
   - 603 REST routes, 110 websocket events, 88 tables, 310 permissions, 263 plugin API methods over Go gob RPC;
   - about 300k lines of Go to port;
   - about 7 new routes a month;
   - 40–70 PM even with Claude, then 0.3–0.5 FTE forever.

   On top of that, the official mobile apps still couldn't get production push, and the official apps can never join E2EE rooms.
2. **Sharpen the wording in PLAN.md**, which is currently "several hundred endpoints". Use the measured numbers, and add the two decisive reasons the plan doesn't state yet: push to the official store apps is gated by Mattermost's license and HPNS terms, and Mattermost plugins require Go gob RPC plus Mattermost's Postgres schema.
3. **Keep option (b-web) as a possible later milestone**, after M2 and only if people without Darkmatter need access:
   - 2–4 PM;
   - the official web app bundle pinned and served by axiond, plus desktop;
   - plaintext channels only, with E2EE channels shown locked to those clients;
   - `PluginsEnabled=false`, no System Console, unlicensed;
   - built against the conformance harness in §4.1.

   Do **not** target the official mobile apps. Mobile stays M4 (native Darkmatter apps with our own relay). This connects to the separate mm-rust/2 research on native apps.
4. **Things worth doing anyway**, since they're cheap and they keep (b-web) possible:
   - Mattermost-style error bodies (`id`, `message`, `status_code`) and an `X-Version-Id` header. Both help Darkmatter and third-party bots too.
   - A read-only direct-Postgres importer as the fallback for plan open question 5 (§2.6).
5. **Fallback if axiond slips badly: (d).** Run Mattermost Team Edition and ship E2EE as a Mattermost plugin plus Darkmatter. That's far cheaper than (a) and keeps every official app working, at the cost of the small-Rust-server goal.

## Also noticed

- This task's worktree branch (`work/1-research-full-rust-rewrite-of-`) was cut before the axiond M0 commit `1f8aa44` on `main`, so `crates/axiond` is missing here. I read it with `git show main:…`.
- `docs/axiond/PLAN.md`, Non-goals: "several hundred endpoints" could be replaced with the measured 603 routes, 110 websocket events and ~7 new routes a month. Its reasons should add HPNS licensing and plugin gob-RPC (see Recommendation 2).
- `docs/axiond/PLAN.md`, Migration: a direct read-only Postgres importer (~12 Mattermost tables) is a fallback the plan doesn't list yet.
- `crates/axiond/src/api/mod.rs` (main): `config/client` returns `Version: "axiond x.y"`, which isn't semver. The official clients compare versions, so this matters only if (b-web) is ever started.
- `dev/seed.sh` uses the unpinned `mattermost/mattermost-preview` image. Pin a tag before using it for conformance recordings.
