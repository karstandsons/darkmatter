# Darkmatter mobile: our own iOS and Android apps

> **Since then (2026-10-23):** phones (M4) are on hold until axiond and Darkmatter on Linux are at 100%. This research is kept for when M4 starts. See [PLAN.md](../PLAN.md#backlog-phones-and-push).

Research for task **RST-2** (mm-rust/2), 2026-10-19. Research only: nothing in the repo was changed. The real server (the team's current Mattermost server) was never contacted.

## Summary

- **Architecture: keep what PLAN.md M4 proposes.** That means a shared Rust core (`darkmatter-core`, built on `mm-api` and `dm-crypto`) exposed through **UniFFI** to a **SwiftUI** app and a **Jetpack Compose** app.
  - It's the one option where the E2EE code, the push code that decrypts in the background, and the plain-Mattermost compatibility all live in one Rust codebase that the desktop app shares.
  - Element X (matrix-rust-sdk), Wire (core-crypto, which is openmls with UniFFI), XMTP (also openmls with UniFFI) and Bitwarden ship this design in production.
  - **Runner-up if UI effort is the constraint:** the same Rust core under **Flutter with flutter_rust_bridge**, so there is one UI. The iOS notification extension still has to be native.
  - **Rejected:**
    - iced or another Rust GUI on mobile: none is production-ready on iOS.
    - Forking mattermost-mobile: 227k lines of TypeScript that aren't ours, and it would force axiond to implement about 169 Mattermost routes.
- **Mattermost API compatibility: don't change the non-goal.** Our mobile app works with plain Mattermost servers through `mm-api`, the same way desktop does. axiond doesn't need to implement Mattermost's API for that to work.
  - Full compatibility means 608 documented operations (775 actual routes), 100 websocket events and 86 tables. That's years of work for one person.
  - **Recommended amendment to PLAN.md:** "axiond implements what Darkmatter **desktop and mobile** use." Mobile adds about 15–25 endpoints (listed in [§6](#6-effort-phasing-and-what-axiond-must-add)).
- **Plain Mattermost servers work for chat but not, by default, for push.** Push only works if the server admin points `PushNotificationServer` at our push relay.
  - Mattermost's hosted push service only signs pushes for Mattermost's own app IDs.
  - Message-ID-only pushes (`id_loaded`) need an Enterprise license.
- **Effort is larger than PLAN.md's "M4: 3–6 months".** My estimate is **about 10–14 person-months** for both apps at MVP-plus-E2EE quality: one developer with Claude, as now. That's roughly 12–16 months of calendar time if the server work continues alongside. Two developers (one per platform, sharing the core) bring it down to about 7–9 months.
  - Android first, then iOS, unless the team is mostly on iPhone. That's an open question.
- **Biggest risks:**
  1. Push with E2EE: iOS's roughly 24 MB memory limit for the notification extension, Apple's filtering entitlement, and FCM priority downgrades.
  2. App Store review for a chat app: report and block, account deletion, and a demo server.
  3. Two UIs maintained by one person.
  4. Android developer verification in 2027 for builds outside Play (F-Droid, sideloading).

---

## 1. Architecture options

### (a) A shared Rust core through UniFFI, with native SwiftUI and Compose UIs (recommended)

```
             ┌──────────── darkmatter-core (Rust) ────────────┐
 SwiftUI app │ mm-api (REST + websocket)   sync engine         │ Compose app
 iOS NSE  ───┤ SQLite cache + outbox       dm-crypto (openmls) ├─── FCM / UnifiedPush receiver
 Share ext.  │ unread/thread logic         markdown → AST      │
             │ emoji, slash commands, built-in plugins          │
             └──────────── UniFFI: Swift + Kotlin bindings ────┘
 Desktop (iced) uses the same crate directly, with no FFI.
```

**How mature UniFFI is:**
- Version 0.32.2 (September 2026), MPL-2.0, still pre-1.0 ("ready for production use… advanced things might break as you upgrade").
- An `async fn` in Rust becomes a Swift `async` function or a Kotlin `suspend fun`. Swift and Kotlin can implement callback traits.
- There's no built-in cancellation, so we'd add our own.
- Sources: https://github.com/mozilla/uniffi-rs, https://mozilla.github.io/uniffi-rs/latest/futures.html

**Real examples:**

| Project | Core | Bridge | Notes |
|---|---|---|---|
| Element X (Matrix) | matrix-rust-sdk, about 441k lines of Rust, Apache-2.0; its `matrix-sdk-ffi` bindings are about 26k | UniFFI 0.32.1 | iOS is about 133k lines of Swift without tests or generated code; Android is about 245k lines of Kotlin without tests. Repos started in 2022. Launched in October 2024, but in November 2025 Element said parity "took longer than expected" and kept "Element Classic" alive ([blog](https://element.io/blog/element-x-and-pro-updates-a-glimpse-into-the-future/)). The apps are AGPL; the SDK stays Apache-2.0. |
| Wire core-crypto | openmls (**Wire's own fork**), rusqlite, about 57k lines of Rust, GPL-3.0 | UniFFI 0.29 for Kotlin and Swift, plus WASM | Shows openmls building and shipping on iOS and Android. https://github.com/wireapp/core-crypto |
| XMTP libxmtp | openmls, MIT | UniFFI | A second, permissively licensed openmls-on-mobile example. |
| Bitwarden | `sdk-internal`, with a `bitwarden_uniffi` crate | UniFFI | https://contributing.bitwarden.com/architecture/sdk/internal |
| Signal | libsignal, AGPL | **Its own** bridge (`rust/bridge/{ffi,jni,node}` via a `bridge_fn` macro), not UniFFI | |
| 1Password | Rust core with native UIs | Its own `typeshare` for types; no evidence of UniFFI (not confirmed) | https://1password.com/blog/typeshare-for-rust |

**Pros:**
- One implementation of sync, crypto and the Mattermost protocol for desktop, iOS, Android and the iOS notification extension.
- Native text input, accessibility, share sheet, notifications and widgets come for free.
- The licenses fit:
  - UniFFI is MPL-2.0, which is file-level copyleft that applies only if UniFFI's own files are modified.
  - openmls is MIT.
  - The core stays MIT OR Apache-2.0.

**Cons:**
- Two UI codebases.
- Binary size. A full chat SDK is big: matrix-sdk-ffi's arm64 `.so` is 64.7 MB stripped (about 24 MB compressed). A focused core like libsignal's is about 9 MB per architecture. Ours should land around 10–20 MB if we keep dependencies lean and avoid duplicate TLS stacks.
- Swift and Kotlin build tooling to maintain: cargo-ndk, xcframeworks.

### (b) iced or another Rust GUI on mobile

| Toolkit | Mobile status, October 2026 |
|---|---|
| iced 0.14 (what desktop uses) | **No official iOS or Android support** and none on the roadmap. Its author has talked about why mobile is hard (SE Radio 713, March 2026). |
| winit | Lists iOS and Android but gives them no support tier. |
| Slint 1.18 | Android since 1.5. iOS is a "tech preview" since 1.12, Rust only. Open IME bug on iOS (#14032). Its royalty-free license requires attribution; otherwise it's GPLv3 or a paid license. |
| Dioxus 0.7 | Mobile runs in the system webview. The native renderer, Blitz, is "work in progress", and push and camera APIs are promised for 0.8. |
| egui 0.37 / Makepad 1.0 | Run on mobile. Robrix (Makepad plus matrix-rust-sdk) still calls itself a work in progress. |

**Verdict:** even the best of these leaves us writing native glue for IME, VoiceOver and TalkBack, push, the notification extension, the share extension and background tasks. That's most of what makes a mobile chat app hard. It's not viable for a production app today. **Re-evaluate in 2027 if iced ships mobile support.**

### (c) Flutter or React Native, with the Rust core through FFI

- **Flutter + flutter_rust_bridge:** 2.13 stable, about 7.9M downloads, a "Flutter Favorite".
  - One Dart UI for both platforms, with good scrolling performance and solid text input and accessibility.
  - The iOS notification extension and share extension must still be Swift, calling the Rust core directly. That's fine, and it's the same as in (a).
- **React Native + uniffi-bindgen-react-native:** npm 0.31.0-6, from Filament, Mozilla and LiveKit. Wire also builds with it.
- **Kotlin / Compose Multiplatform:** iOS has been stable since 1.8 (May 2025) and adds about 9 MB.
  - It needs Kotlin/Native bindings to Rust. UniFFI's Kotlin output is JVM-only, so this needs a third-party generator such as Gobley. That's an extra moving part.

**Pros:** one UI codebase, so roughly 30–40% less UI work for two platforms.

**Cons:**
- A third language and toolchain (Dart or JS) on top of Rust, Swift and Kotlin.
- The desktop app stays iced, so the UI isn't shared with desktop either way.
- Flutter's iOS feel is close to native but not native.

### (d) Fork mattermost-mobile and add E2EE

- **License:** `LICENSE.txt` is stock Apache-2.0 with no added exceptions, so a fork is legal. The "Mattermost" name and logo need written permission, so a fork must rebrand.
- **Size:** v2.45.0 (2026-10-15) is React Native 0.83, Expo 55 and WatermelonDB. It has **227k lines of non-test TypeScript in `app/`**, 180k lines of tests and about 27k lines of native code.
- **What axiond would have to serve:** the app's REST client resolves to **about 169 distinct method-and-route pairs**. axiond M0/M1 targets 35.
  - The app also hard-blocks servers older than 9.11.0 (`app/constants/supported_server.ts`).
- **E2EE:** none. The only earlier attempt, Quarkslab's `mattermost-plugin-e2ee`, was webapp-only, last released in May 2022 and leaks messages on Mattermost ≥6.6. It's dead.

**Verdict:** this gives the fastest feature-complete app against **plain** Mattermost. But:
- We'd inherit a codebase five times the size of everything we have, and it changes monthly.
- axiond would have to grow to about 169 routes.
- E2EE, MLS state and decrypting in the notification extension would still need a Rust core through uniffi-bindgen-react-native.
- That's option (c) plus a large fork we don't control. **Rejected.**

### Should we chase full Mattermost API compatibility after all?

| Mattermost (main @ `9df98424`, 2026-10-19) | Count | How I got it |
|---|---|---|
| Documented REST operations | **608 on 485 paths** | Parsed the 59 files in `api/v4/source/*.yaml` with PyYAML |
| Routes actually registered | **775** | `grep -rhE '\.Methods\(' server/channels/api4` (non-test) |
| Websocket event types | **100** | `WebsocketEvent*` constants in `server/public/model/websocket_message.go` |
| SQL tables | **86 live** (88 created, 4 dropped, across 233 migrations) | `CREATE TABLE` in `server/channels/db/migrations/postgres/*.up.sql` |
| Server Go, non-test | 535k lines (39k in `api4` handlers) | `wc -l` |
| Routes the mobile app uses | about 169 | Static scan of `doFetch` in mattermost-mobile `app/client/rest/*.ts` |
| Routes Darkmatter uses today | 35 REST + 12 websocket events | PLAN.md Appendix A |
| axiond on main | 7 tables, about 2.3k lines of Rust | `migrations/0001_init.sql`, `wc -l` |

**What it would buy us:** the stock Mattermost apps would work against axiond. **What it wouldn't:**
- **Push:** Mattermost's hosted proxy only signs for Mattermost's own bundle IDs ([docs](https://docs.mattermost.com/deployment-guide/mobile/host-your-own-push-proxy-service)).
- **E2EE:** the stock apps would never speak our extension.

Serving even just mobile's 169 routes is about 4–6 person-months of server work, and that only buys a non-encrypted app without push. **Keep the non-goal.**

---

## 2. What existing Darkmatter code can be shared

Line counts are from `wc -l` on each `.rs` file, tests included. Darkmatter is 11,527 lines and `mm-api` is 1,113 lines of source (1,661 including tests and the example).

| Goes into `darkmatter-core` | Lines | Changes needed |
|---|---|---|
| `mm-api` (client, models, websocket, errors) | 1,113 | Already UI-free; main has already split out a models-only feature for axiond.<br>`tokio-tungstenite`'s `rustls-tls-native-roots` relies on `rustls-native-certs`, which **doesn't support Android**, so it should switch to `rustls-platform-verifier` (reqwest 0.13 already uses it).<br>Add the mobile endpoints from [§6](#6-effort-phasing-and-what-axiond-must-add). |
| `store.rs`: unread, ordering, dedup | 647 | Its only iced dependency is `iced::widget::markdown::parse`. Swap in `pulldown-cmark` and expose an AST the native UIs render. |
| `text.rs`, `commands.rs` (the command registry; key bindings stay desktop-only) | 572 | Small changes. |
| `session.rs`, `config.rs` | 442 | Keyring and paths go behind a trait that the platform implements: Keychain or Android Keystore. |
| `plugins.rs`: manifest and JSON protocol | 879 | The protocol moves into `dm-plugin`. **The runtime can't move:** it spawns subprocesses (`tokio::process::Command`, python3, git). See below. |
| `media.rs` (the cache logic only) | 224 | Drop `iced::widget::image`; keep the fetch-once cache, backed by disk on mobile. |
| **Subtotal, portable now** | **about 3.9k** | About 2.8k from Darkmatter plus 1.1k from `mm-api`. |
| The logic in `chat.rs` | about 1.2k of 2,999 | `Chat::update` is about 1,300 lines that mix state changes with `iced::Task` and scroll operations. They need refactoring into a headless state machine that returns effects. This also helps desktop, as PLAN.md says. |

| Stays desktop-only (iced views, desktop features) | Lines |
|---|---|
| `app`, `chat_view`, `panels`, `pane`, `layout`, `lightbox`, `zoom`, `viewer`, `style`, the pickers, `switcher`, `shortcuts`, `plugin_manager`, `main` | about 5.2k |
| `update.rs` (the self-updater; the stores handle updates on mobile) | 518 |
| The view glue in `chat.rs` | about 1.8k |
| The `omarchy-theme` crate (a mobile theme could still read its palette files) | 942 |

**The split:** about 40% of today's client code (about 5.1k of 12.6k lines, counting `chat.rs`'s logic) moves into the core, and about 60% stays as iced UI.

**The core will mostly be new code.** Mobile needs a lot that Darkmatter's in-memory store never needed:
- a SQLite cache;
- an outbox for messages sent offline;
- sync since a cursor;
- push handling;
- a lock shared between processes so the app and the iOS notification extension don't clash;
- `dm-crypto`;
- the UniFFI surface.

I expect the core to grow to **about 12–20k lines** (matrix-rust-sdk is far larger at 441k), and each native UI to be **about 15–25k lines**.

**Plugins on mobile:**
- The subprocess model isn't possible on iOS, which has no fork/exec in the sandbox. App Store guideline 2.5.2 also forbids downloading executable code, and Play has a similar policy.
- **Plan:**
  - Reimplement the built-in plugins in Rust in the core: `gif` (it already uses the server's Giphy key), `kaomoji`, `quote`, and probably `meme`.
  - User plugins on mobile become **server plugins**. PLAN.md already plans `where = "server"` with the same protocol.
  - A WASM sandbox might come later, but its fit with guideline 2.5.2 needs care.

---

## 3. Push notifications with E2EE

**Recommended design** (it extends PLAN.md's "wake-up only" relay):

1. **Device registration.** The app registers its token with axiond (`PUT users/sessions/device`, kept compatible with Mattermost: `<platform>-v2:<token>`) along with its MLS device ID.
2. **Sending.** On a new message, axiond sends `{token, opaque_id, server_id}` to **`axiond-push`**, a small relay we run that holds the APNs `.p8` key and the FCM HTTP v1 service account.
   - The relay sees only tokens and timing.
   - The payload has no sender, no channel and no text.
   - Validate the payload signature the way Mattermost does: an ES256 JWT carrying `ack_id` and `device_id`.
3. **iOS: a Notification Service Extension (NSE).**
   - It opens the shared core: an app group container for SQLite, plus a shared keychain access group for the session and device keys.
   - It fetches the message by ID over TLS, decrypts it with the MLS state, and rewrites the notification.
   - **Its limits are about 24 MB of memory and about 30 s**, enforced by the system. These are Apple forum and DTS figures, not a published spec ([forum](https://developer.apple.com/forums/thread/721884)).
   - So the core needs a mode for the NSE: no websocket, no full sync, a decrypt path only, and a lock shared with the app. Element X does exactly this with matrix-rust-sdk's `NotificationClient`.
   - Suppressing a notification (for example a commit or a read receipt) needs the **`com.apple.developer.usernotifications.filtering`** entitlement. Apple approves it by hand, and forum reports describe rejections and waits of months.
   - **Design so that every push produces a visible notification**, with "New message" as the fallback, rather than depending on that entitlement.
4. **Android with FCM: high-priority data messages.**
   - FCM **downgrades an app's high-priority messages** to normal priority, judged over 7 days, if they don't lead to a visible notification ([docs](https://firebase.google.com/docs/cloud-messaging/android-message-priority)).
   - So only push for things that will notify, and send silent sync as normal priority.
   - The decrypt work runs in the receiver or a short expedited job. Android 15 caps `dataSync` foreground services at 6 h per 24 h.
5. **Android with UnifiedPush** (the F-Droid build): WebPush to the user's distributor (ntfy, NextPush, Sunup).
   - Element X, Molly, Tusky, FluffyChat and DAVx⁵ ship it ([list](https://unifiedpush.org/users/apps/)).
   - axiond can send WebPush directly, with no relay.
6. **What the push providers still see:** device token, app ID, timing and payload size. Padding or batching is a later hardening item, as PLAN.md says.

**Plain Mattermost servers:**
- The payload has `ack_id`, `post_id`, `channel_id`, `message`, `sender_name`, `version: "v2"` and so on (`server/public/model/push_notification.go`). The server only accepts device platforms on an allowlist: `apple_rn`, `apple_rnbeta`, `android_rn`, `apple_oxide`, `android_oxide` (`session.go`).
- **`id_loaded` (ID-only push, then fetch the content through `POST notifications/ack`) needs an Enterprise license.** Otherwise the server quietly falls back to `generic` (`notification_push.go:806`).
- **So our app gets pushes from a Mattermost server only if:**
  - the admin sets `PushNotificationServer` to our relay;
  - our app registers under an allowlisted platform name;
  - our relay handles the Mattermost proxy API (`/api/v1/send_push`, `/api/v1/ack`).
- That also takes push away from the official Mattermost apps on that server, unless our relay forwards unknown tokens upstream, which isn't permitted for non-customers.
- **Recommendation:** on plain Mattermost, support foreground websocket sync plus an optional relay setting for admins who opt in. Document clearly that push is "axiond-native".
- **Unverified:** whether HPNS or TPNS is usable by Team Edition servers at all. The page didn't load for me.

---

## 4. Store and distribution

| Item | What it requires | Source |
|---|---|---|
| Apple Developer Program | $99/yr. An organization account (Karst & Sons) needs a D-U-N-S number. | developer.apple.com (fee from memory) |
| **App Store guideline 1.2 (user-generated content)** | A content filter, **reporting content with timely responses** (rejections cite 24 h), **blocking users**, and published contact details. **axiond doesn't plan report or block yet.** Self-hosted servers report to their own admin; we document the process and send reports to the server admin. | [Guidelines](https://developer.apple.com/app-store/review/guidelines/), [forum](https://developer.apple.com/forums/thread/807358) |
| Guideline 5.1.1(v) | If the app lets you create accounts, it must let you delete them in the app. axiond needs `DELETE` on users/me, or a "request deletion" flow. | Guidelines |
| Guideline 2.1(a) | A demo account on a **live** backend. A self-hosted-only app was rejected without one. **We have to run a demo axiond.** | [Forum](https://developer.apple.com/forums/thread/815919) |
| Guideline 2.4.1 | iPhone apps should run on iPad. | Guidelines |
| TestFlight | 10,000 external testers; builds last 90 days; the first external build goes through beta review (about 24–48 h). | [TestFlight](https://developer.apple.com/testflight/) |
| Export compliance (iOS) | openmls is standard cryptography that isn't part of the OS. Set `ITSAppUsesNonExemptEncryption`. Since the 2021 BIS rule, open-source mass-market code needs no email notice or annual report. **France still needs an ANSSI declaration**, or we exclude France at first. | [Apple](https://developer.apple.com/help/app-store-connect/reference/export-compliance-documentation-for-encryption/), [Baker McKenzie](https://sanctionsnews.bakermckenzie.com/bis-updates-reporting-requirements-relating-to-mass-market-encryption-items-and-publicly-available-software-and-also-updates-certain-classifications/) |
| Google Play | $25 once. **New personal accounts need 12 testers opted in for 14 days** before production; organization accounts are exempt. Data safety form. Play App Signing with AAB. targetSdk 36 required from 2026-08-31, which also forces edge-to-edge. | [Testing rule](https://support.google.com/googleplay/android-developer/answer/14151465), [target SDK](https://developer.android.com/google/play/requirements/target-sdk) |
| Android developer verification | Enforced from 2026-09-30 in Brazil, Indonesia, Singapore and Thailand; **global in 2027**. Sideloaded apps on certified devices need a verified developer. There's a no-fee limited tier (20 devices) and an "advanced flow" for power users. Register Karst & Sons early. | [developer.android.com](https://developer.android.com/developer-verification), [F-Droid letter](https://f-droid.org/2026/02/24/open-letter-opposing-developer-verification.html) |
| F-Droid | Free toolchain only, so **a `foss` build flavor without Firebase**, using UnifiedPush. F-Droid builds from source, Rust included (Element X and FluffyChat do this). | [Inclusion policy](https://f-droid.org/docs/Inclusion_Policy/) |
| EU alternative store | AltStore PAL; apps still need Apple notarization. A later option. | [mjtsai](https://mjtsai.com/blog/2025/10/08/evolving-altstore-pal/) |
| Licensing in the stores | Keep the mobile apps **MIT OR Apache-2.0**, like the core. App Store terms and GPL/AGPL are a known conflict, which is one reason PLAN.md keeps the client permissive. Don't pull AGPL code (libsignal, Element X UI code) into the apps. | PLAN.md, Element X LICENSE |

---

## 5. Mobile-specific features

| Feature | How | Notes |
|---|---|---|
| **Background sync** | iOS: none you can rely on (`BGAppRefreshTask` runs when the system chooses), so use push plus the NSE, and sync on foreground. Android: FCM or UnifiedPush plus expedited WorkManager jobs; no permanent foreground service. | On plain Mattermost without our relay: sync only while the app is open. |
| **Offline cache** | SQLite in the core (rusqlite or sqlx; openmls has an sqlx storage provider since 0.8, so crypto and cache can share one database file or keep two). Cache recent pages per channel, an outbox with retry, and media on disk with a size cap. SQLCipher with a key from the Keychain or Keystore. | |
| **Share sheet** | iOS Share Extension (about 120 MB memory) using the core to upload; Android `ACTION_SEND` intent. | Mattermost's share extension is 3.3k lines. |
| **Deep links** | `darkmatter://` plus universal links and App Links. These need `apple-app-site-association` and `assetlinks.json` on the **server's** domain, which works for axiond but not for links to plain Mattermost servers. | axiond serves both files. |
| **Biometrics** | An optional app lock: LocalAuthentication or Keychain with `biometryCurrentSet` on iOS; BiometricPrompt with Keystore keys that need user authentication on Android. | |
| **Accessibility** | VoiceOver and TalkBack labels on messages, reactions and composer actions; Dynamic Type and font scaling. This is the main reason to use native UI. | |
| **Tablets** | A split view: channel list plus timeline plus thread. iPad is expected by guideline 2.4.1. | |
| **Images, files, GIFs** | System photo picker, camera, file picker; thumbnails come from the server (non-E2EE) or the client (E2EE). GIF search through the server's Giphy key as on desktop; animated GIF and WebP rendering is native. | |
| **E2EE UX** | Safety numbers and QR verification need the camera; device approval; recovery key entry; "new device" banners. | Shares `dm-crypto` with desktop. |
| CarPlay / Android Auto | Not needed. | |

---

## 6. Effort, phasing, and what axiond must add

**Assumptions:**
- One developer working with Claude, the pace PLAN.md assumes.
- Estimates are in person-months (pm) and are rough, based on the sizes above.
- Mobile phases run after or alongside server M1–M3. Mobile E2EE needs M3's `dm-crypto`.

| Phase | Scope | Effort |
|---|---|---|
| **P0: core extraction** | `darkmatter-core` crate: store, headless chat state machine, markdown AST, platform traits (keystore, paths, notifications), SQLite cache and outbox, built-in plugins in Rust, TLS fix. Desktop moves onto it. UniFFI surface, Swift Package and AAR builds in CI. | 2–2.5 pm |
| **P1: Android MVP** | Compose. Login (password and MFA, plus "add a server": axiond or Mattermost), teams, channels and DMs, timeline with markdown, threads, compose, edit and delete, reactions, emoji, files and images (upload and view), GIF picker, slash commands with autocomplete, unreads and mentions, search, foreground websocket. FCM through `axiond-push`; a `foss` flavor with UnifiedPush. Internal testing on Play, then the 12-tester closed test. | 2.5–3 pm |
| **P2: iOS MVP** | SwiftUI with the same scope; NSE (non-E2EE first); TestFlight; demo server; App Store review (report and block, account deletion). | 2.5–3 pm |
| **P3: E2EE on mobile** | MLS through the core, decrypting in the NSE and the Android receiver, a lock shared between processes, device list and approval, QR and safety-number verification, recovery key, encrypted files, local search index. | 2–3 pm (after server M3) |
| **P4: polish** | Share extension and intent, deep links, biometric lock, tablet layouts, accessibility pass, offline hardening, Mattermost push relay for admins who opt in, F-Droid submission. | 1.5–2 pm |
| **Total** | | **about 10.5–13.5 pm** |

Scaled to compare: Mattermost's app is 227k lines of non-test TypeScript, and Element X took more than 3.5 years with a much larger team to reach broad parity. Our scope is far narrower and the core is shared, so a year of focused work is plausible. PLAN.md's 3–6 months is not.

**MVP:** the P0–P2 scope, roughly 7–8.5 pm. Non-E2EE chat on axiond and plain Mattermost, with push on axiond. E2EE comes in P3.

**Team options:**
- One developer: about 12–16 calendar months alongside server work.
- Two (Android and iOS in parallel after P0): about 7–9 months.

**What axiond must add for mobile** (about 15–25 endpoints, around 1–1.5 pm, most of it planned already):
1. **Short-lived access tokens plus refresh tokens** (planned in PLAN.md): `POST users/login` returns both; add `POST users/sessions/refresh`. Mattermost has no refresh-token flow, so this is axiond-only and the client falls back to long-lived tokens on Mattermost.
2. **Device registration:** `PUT users/sessions/device`, Mattermost-compatible, linked to the MLS device ID.
3. **Push sender** to `axiond-push` (APNs and FCM through the relay) and **WebPush** for UnifiedPush; per-device notification preferences; badge counts.
4. **Fetch by push ID** (Mattermost does this with `POST notifications/ack`): returns the encrypted message for the NSE in one small call.
5. **Sync since a cursor:** posts changed since a timestamp per channel (`GET channels/{id}/posts?since=` is already Mattermost-shaped), channel members changed since, and replay of websocket events after a sequence number (M0 already has resume).
6. **Report, block and mute users; delete account** (App Store 1.2 and 5.1.1(v)); an admin view for reports.
7. **Image thumbnails and previews at mobile sizes**, and ranged downloads for large files.
8. `apple-app-site-association` and `assetlinks.json`; a **public demo instance** for app review.
9. The `axiond-push` relay itself (Rust, small; Mattermost's own push proxy is about 2.1k lines of Go), plus optional Mattermost proxy-API compatibility.

---

## 7. Key risks

| Risk | Likelihood and impact | Mitigation |
|---|---|---|
| The NSE's roughly 24 MB limit and the shared database cause crashes or missed notifications | High / High | A lean core build for the NSE (no websocket or media), a lock shared between processes, a test harness that measures memory. Element X solved the same problem. |
| Apple denies or delays the filtering entitlement | Medium / Medium | Always show a notification, with a "New message" fallback; never rely on silent pushes. |
| FCM downgrades high-priority messages | Medium / Medium | Send high priority only for messages that will notify. |
| App Store rejection (guideline 1.2, demo account, 5.1.1(v)) | High on the first submission / Medium | Build report, block and delete in P2; run a demo server; write review notes. |
| One person maintaining two UIs | High / High | Keep the UIs thin and the core fat; if this becomes the bottleneck, switch the second platform to Flutter on the same core rather than dropping it. |
| UniFFI is pre-1.0; we depend on the openmls API | Medium / Low–Medium | Pin versions and keep the FFI surface small and coarse. Wire forks openmls; we should avoid that. |
| Plain Mattermost: no push, and server version drift (Mattermost v12.0 shipped on 2026-10-14) | Certain / Medium | Say clearly that mobile push is axiond-native; contract-test `mm-api` against a local Mattermost Team Edition in CI. Mattermost's server allowlist already includes `apple_oxide` and `android_oxide`, which hints that Mattermost is building a new native client of its own. |
| Android developer verification (global in 2027) and F-Droid's future | Medium / Medium | Register the organization early; keep the UnifiedPush flavor buildable. |
| Binary size | Low / Low | Lean dependencies, one TLS stack, LTO, `opt-level="z"` for mobile. |

---

## Recommendation

1. **Keep PLAN.md's architecture:** a Rust `darkmatter-core` through UniFFI, with SwiftUI and Compose UIs and iced on desktop.
2. **Change the M4 estimate** from 3–6 months to about 10–14 person-months, split into P0–P4 as above.
3. **Keep "no full Mattermost API compatibility".** Amend the scope to "what Darkmatter desktop and mobile use", adding the axiond list in [§6](#6-effort-phasing-and-what-axiond-must-add).
4. **Start P0 now, in small steps.** It pays off on desktop right away by making `chat.rs` testable, and it's what everything on mobile depends on.
5. **Pull the App Store prerequisites into server M1/M2:** report and block, account deletion, refresh tokens, device registration. They're cheap now and block review later.

**Open questions for the human:**
- Are most of the team and friends on iPhone or Android? That decides which app comes first.
- Will you run the public push relay and a demo server?
- Does the team's Mattermost admin agree to point push at our relay during the overlap?

## Also noticed

- `crates/mm-api/Cargo.toml`: `tokio-tungstenite` uses `rustls-tls-native-roots`, which relies on `rustls-native-certs` and doesn't support Android. Switch to `rustls-platform-verifier` before any mobile build.
- `crates/darkmatter/src/store.rs`: message parsing depends on `iced::widget::markdown`. A UI-free core needs `pulldown-cmark` (or similar) directly.
- `docs/axiond/PLAN.md` (Security baseline, Roadmap M1/M2): there's no report, block or account-deletion feature. App Store guidelines 1.2 and 5.1.1(v) require them for the mobile apps.
- `docs/axiond/PLAN.md` (Mobile and push): the "wake-up only" design should account for FCM downgrading high-priority messages that show no notification, and for the iOS filtering entitlement needing Apple's approval.
- `docs/axiond/PLAN.md` (Roadmap M4): the 3–6 month estimate is low; this report gives about 10–14 pm.
- `docs/plugins.md`: the plugin model (subprocesses, python3, git install) can't work on iOS or under Play policy. Mobile needs built-in Rust versions of the built-in plugins, plus server plugins.

## Method and sources

- **Local code:** `wc -l` per file on this worktree; axiond and `mm-api` read from `main` with `git show` (axiond is on main, ahead of this branch).
- **Upstream code:** shallow clones taken on 2026-10-19 of mattermost/mattermost `9df98424`, mattermost-mobile `cd1ee53`, mattermost-push-proxy `20f2a04`, quarkslab/mattermost-plugin-e2ee, matrix-rust-sdk, element-x-ios and element-x-android, wireapp/core-crypto and signalapp/libsignal. Counts come from `wc -l`, grep, and PyYAML parsing of the OpenAPI spec. No code from these repos was run.
- **Facts about stores, platforms and push:** the official docs and forum threads linked inline.
- **Unverified items:**
  - Element X's team size;
  - whether HPNS or TPNS can be used by Team Edition servers;
  - the exact design of Element X's shared keychain for the NSE;
  - the current ANSSI form;
  - 1Password's bridge mechanism.
