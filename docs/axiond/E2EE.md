# End-to-end encryption: design

**Status: draft, M1 (2026-10-19).** This is the design that M1's features are built around and that M2 implements. It's written to be read and criticised by people outside the project. [PLAN.md](PLAN.md) has the goals and the reasons for MLS; this document says exactly how it works.

Contents:
1. [Goals and limits](#1-goals-and-limits)
2. [Building blocks](#2-building-blocks)
3. [Identities and devices](#3-identities-and-devices)
4. [Groups](#4-groups)
5. [The server as delivery service](#5-the-server-as-delivery-service)
6. [Messages](#6-messages)
7. [Files](#7-files)
8. [Reactions, edits, deletes, threads](#8-reactions-edits-deletes-threads)
9. [Mentions, unreads and notifications](#9-mentions-unreads-and-notifications)
10. [Search](#10-search)
11. [Calls](#11-calls)
12. [New devices, verification and recovery](#12-new-devices-verification-and-recovery)
13. [Storage on the client](#13-storage-on-the-client)
14. [What a malicious server can and can't do](#14-what-a-malicious-server-can-and-cant-do)
15. [API](#15-api)
16. [Testing](#16-testing)
17. [What M1 builds because of this](#17-what-m1-builds-because-of-this)
18. [Decisions](#18-decisions)

---

## 1. Goals and limits

**Protected:** the content of messages, edits, reactions, files and file names, thumbnails, link previews and calls in encrypted channels. Neither the server operator, the VPS provider, nor anyone who gets a copy of the database or a backup can read them.

**Not protected, and said so in the app:** metadata. The server knows who is in which channel, who sent something when, roughly how big it is, who is in a call and for how long, and which users a message mentions (see [§9](#9-mentions-unreads-and-notifications)).

**Where encryption applies** (from PLAN.md): DMs and group DMs always; private channels by default; public channels when an admin turns it on. Turning it on affects new messages only. Turning it off needs an admin and posts a visible system message, and clients show a warning.

**Rules we hold ourselves to:**
- **No crypto of our own.** Everything is MLS ([RFC 9420](https://www.rfc-editor.org/rfc/rfc9420)) through openmls, or a named construction from a reviewed crate. Every such choice is listed in [§2](#2-building-blocks).
- **The server is never trusted for content or for keys.** It's trusted for availability and ordering only, and clients check what they can (see [§14](#14-what-a-malicious-server-can-and-cant-do)).
- **Everything the server stores for encryption is either public or ciphertext.**

## 2. Building blocks

| Purpose | Choice | Notes |
|---|---|---|
| Group key agreement | **MLS** via **openmls 0.9** (MIT) | Confirmed at the start of M2: openmls has had an independent audit (SRLabs, 2026) and mls-rs hasn't. |
| Crypto provider | `openmls_libcrux_crypto` (formally verified primitives), else `openmls_rust_crypto` | Pick the one openmls recommends at M2. |
| Ciphersuite | `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (0x0001) | The one every MLS implementation must support. A post-quantum suite (an X-Wing KEM) is added once openmls ships it as stable; groups record their suite, so old and new groups can coexist. |
| User identity key | Ed25519 (`ed25519-dalek`) | Signs device certificates ([§3](#3-identities-and-devices)). |
| File encryption | XChaCha20-Poly1305 in the **STREAM** construction (`chacha20poly1305` + `aead::stream`, RustCrypto) | Chunked, so big files stream and truncation is detected. |
| Key derivation | HKDF-SHA256 (`hkdf`) | For deriving backup keys from the recovery key. |
| Passphrase stretching | argon2id (`argon2`, already used by axiond) | Only for the optional passphrase backup. |
| Call media | SFrame ([RFC 9605](https://www.rfc-editor.org/rfc/rfc9605)), keys from the MLS exporter | Details in [§11](#11-calls) and the calls research. |
| Randomness | The OS (`getrandom`) | Nothing seeded by hand. |

Everything above is MIT or Apache-2.0, so it fits the permissive client and the AGPL server.

## 3. Identities and devices

Two layers, like Matrix's cross-signing and Signal's linked devices:

- **A user identity key** (Ed25519), one per user. Its public half is what people verify (safety numbers, QR codes). Its private half lives on the user's devices, and in the encrypted backup if they made one ([§12](#12-new-devices-verification-and-recovery)).
- **A device**, one per installation of Darkmatter (desktop or phone). Each device has:
  - a random **device ID** (26 characters, like other IDs), tied to its login session;
  - an **MLS signature key** (Ed25519), generated on the device and never leaving it;
  - a **device certificate**: `{user_id, device_id, device_signature_public_key, created_at}` signed by the user identity key.

**MLS credentials** are `BasicCredential`s whose identity is `user_id:device_id`. A client accepts a group member only if:
1. the server can show a device certificate for that `user_id:device_id`;
2. the certificate's signature verifies against that user's identity key;
3. the certificate's signature key matches the member's leaf signature key in the MLS tree;
4. that identity key is the one this client has seen for the user before (trust on first use), or the one the user verified.

So the server can't add a device of its own making: it has no user identity key to sign one with.

**Changes to a user's identity key** only happen when someone loses every device and has no backup. Clients show a warning in every conversation with that user. **In a conversation where the user was verified, sending is blocked** until the change is accepted. (Signal only warns; the stricter rule fits our priorities. Decided, [§18](#18-decisions).)

**A device is a signed-in session** (as built in M2): signing out, a revoked or expired session, a password reset or deactivation revokes the device and takes it out of every group. Signing in again makes a new device.

**Removing a device** (signing it out from Account → Signed-in apps, or because a session is revoked): the server ends its sessions, stops delivering to it, and proposes its removal from every group ([§4](#4-groups)). As built, revocations aren't signed by the identity key: the server can already refuse to deliver to any device, so a signature wouldn't stop a malicious one from cutting a device off, and it can't add one.

## 4. Groups

- **One MLS group per encrypted channel.** The MLS group ID is the channel ID.
- **Members are devices, not users.** Every device of every channel member is in the group. A user's own devices are members too, so what you send appears on your other devices.
- **Handshake messages (proposals, commits) are sent as MLS `PublicMessage`s; application messages as `PrivateMessage`s.**
  - Handshake messages carry membership changes, which the server knows anyway. Making them public lets the server check them with openmls's `PublicGroup` ([§5](#5-the-server-as-delivery-service)): that commits come from current members, that they're for the current epoch, and that adds are for real members of the channel.
  - Content is always a `PrivateMessage`.
- **The server is an MLS "external sender"** (the `external_senders` group extension) with its own signing key. It may propose **Remove** only. Clients reject any other proposal from it. That lets removals happen even when the person who removed someone is offline.
- **Membership changes:**
  - **Someone is added** (an invite, or opening a DM): the client making the change claims a key package for each of the new person's devices and commits the Adds. New devices get an MLS `Welcome`.
  - **Someone joins a public encrypted channel by themselves:** their device joins with an **external commit**, using the group's `GroupInfo` from the server. The server only hands `GroupInfo` to channel members.
  - **Someone leaves or is removed:** the server proposes Remove for each of their devices. The next member to send anything **must commit pending proposals first** (clients do this automatically), and the server refuses application messages for an epoch that still has pending removals older than a few seconds. In the meantime the server has already stopped delivering to the removed devices.
  - **A user adds a device:** the new device joins each of the user's groups with an external commit, once it has a device certificate ([§12](#12-new-devices-verification-and-recovery)).
- **Updates.** Each device commits an update of its own leaf at least every 7 days and after it has been offline for a day, for post-compromise security.
- **Catching up** checks old commits against the certificates of the devices they added, including devices revoked since (the server keeps listing them, marked revoked; nothing new is added from a revoked one). A device that can't apply a handshake drops its copy of the group and joins again by external commit, rather than carry on out of step.
- **Old epochs.** Clients keep the secrets of the last few epochs (openmls `max_past_epochs`, starting at 5) so messages sent just before a commit still decrypt.
- **The UI shows membership from the MLS group**, not from the server: "Bob's phone joined" and "Alice removed Carol" come from processed commits. If the server's member list and the group disagree for more than a short while, the channel shows a warning.

## 5. The server as delivery service

axiond takes the MLS **Delivery Service** role. It also uses openmls's `PublicGroup` to track the public state of every encrypted group (the tree and epoch, no secrets).

**Ordering.** Per group, the server keeps a sequence of handshake messages. A commit is accepted only if it's for the group's current epoch and `PublicGroup` validates it. The first valid commit for an epoch wins; others are refused with `409` and the losing client fetches the new commit, re-applies its change and tries again.

**Storage** (all public or ciphertext):
- each device's **key packages** ([§15](#15-api)): one-time ones plus one last-resort package;
- per group: the **handshake log** (commits and proposals, in order), the latest **`GroupInfo`**, and pending **`Welcome`s** per device;
- **device certificates**, revocations and **user identity public keys**;
- **encrypted backups** ([§12](#12-new-devices-verification-and-recovery));
- **posts** whose content is ciphertext ([§6](#6-messages)).

**Delivery.** Handshake messages and Welcomes go to devices over the websocket (`mls_commit`, `mls_welcome`) and can be fetched by sequence number after a disconnect. The log holds the server's Remove proposals in order with the commits, because a commit refers to proposals its readers must already have. A device moves its place in the log to the sequence number the server returns for its own commit, so it never reads its own commit back. The handshake log is kept for **30 days**. A device that has been away longer re-joins its groups with an external commit; it can't read what was sent while it was away unless it has a history backup.

**What the server checks** (so a buggy or malicious client can't break a group for everyone):
- the sender of a commit is a current member device, and the device belongs to the logged-in session;
- every Add is for a device of a user who is a channel member, with a valid certificate;
- the server's own proposals are only Removes;
- application messages are for the current epoch (or one of the few before it).

## 6. Messages

An encrypted post uses the normal `POST /api/v4/posts`:

```json
{
  "channel_id": "…",
  "root_id": "…",
  "message": "",
  "props": {
    "dm_e2ee": { "v": 1, "epoch": 42, "ct": "<base64 MLS PrivateMessage>" },
    "dm_mentions": ["<user id>", "…"]
  },
  "file_ids": ["…"],
  "pending_post_id": "…"
}
```

- **`message` is empty.** axiond fills in "🔒 Encrypted message" for any client that doesn't understand `dm_e2ee`, and refuses posts with any other text in an encrypted channel, so plaintext can't slip into an encrypted room. It also refuses ciphertext from an epoch more than a few behind, and sending while its removal proposals have waited more than 30 seconds for a commit.
- **Read in order.** MLS keeps keys for only so many skipped messages (20 per sender) and past epochs (5). So a device reads what it missed oldest first, from the newest message it had read in each channel, before decrypting anything newer. It catches up on handshakes only as far as each message's epoch. It does this once per connection, and again after the connection drops. Sending and new handshakes wait for it.
- **Read once.** MLS can decrypt a message only once, so a device keeps each decrypted text in its local database and reads it from there afterwards. A sender can't decrypt its own message at all, so it keeps the text when it encrypts (under the post's `pending_post_id` until the server numbers it). Posts carry `pending_post_id` to every reader, for the authenticated-data check.
- **Where it's built:** `dm-client` holds the client side (setup, groups, encrypting and decrypting posts) over `mm-api`, so Darkmatter and the phone apps share it; the app supplies its storage. Server commands that post for you (`/msg @someone text`) don't write into encrypted conversations.
- **The MLS plaintext** is a small JSON document:

  ```json
  { "t": "post", "message": "the text", "files": [ { "id": "…", "key": "…", "name": "…", "type": "…", "size": 0, "sha256": "…", "thumb": { … } } ], "preview": { … } }
  ```

  Link previews, when the sender has them on, are fetched by the sender's client and travel inside the plaintext.
- **The MLS authenticated data** (signed but not encrypted) binds the ciphertext to its place: `channel_id`, `root_id`, `pending_post_id` and the kind of event. A server that moves a ciphertext to another channel or thread, or replays it as an edit, is detected.
- **Server-visible:** channel, thread (`root_id`), sender device, time, ciphertext size, file IDs and sizes, and the mention list.
- **Padding:** plaintext is padded to a multiple of 256 bytes before encryption (MLS supports padding), so the size reveals less.

## 7. Files

1. The client picks a random 256-bit key and encrypts the file with XChaCha20-Poly1305 STREAM, in 64 KiB chunks.
2. It uploads the ciphertext with the normal `POST /api/v4/files`, with a neutral file name (`blob`) and type `application/octet-stream`.
3. For images and video, it makes a thumbnail and preview itself, encrypts them the same way, and uploads them as separate blobs.
4. The keys, real name, type, size, SHA-256 of the ciphertext, and thumbnail details go inside the encrypted post ([§6](#6-messages)).

The server sees only blob sizes. It never makes thumbnails for these files, and serves them as `application/octet-stream` downloads.

As built (step 6):
- **Format:** `DMF1`, a random 19-byte nonce prefix, then 64 KiB chunks, each with its 16-byte tag (RustCrypto's `aead::stream`, big-endian 32-bit counter with the last-chunk flag). A file that divides evenly ends on a full chunk marked last; an empty file is one empty last chunk.
- **Checks on download:** the SHA-256 of the ciphertext must match the post's, then every chunk must authenticate and the last must be marked last. A swapped, changed or cut-off file is refused.
- **Thumbnails:** pictures (PNG, JPEG, GIF, WebP, by their bytes, not their name) get a JPEG thumbnail at most 800x600, uploaded as a second blob. Both uploads are attached to the post, so axiond allows 20 per encrypted post.
- **Before sending,** the keys wait in the outbox (in the encrypted local database) as `props.dm_files`; `encrypt_post` moves them inside the ciphertext and refuses if a file they name isn't attached. A post with `dm_files` is never sent to a channel that isn't encrypted.
- **Opening a file in another app** writes it decrypted to Darkmatter's cache folder (mode 0600), since that app has to read it.

## 8. Reactions, edits, deletes, threads

- **Edits:** `PUT /api/v4/posts/{id}/patch` with a new `dm_e2ee`. The plaintext has `"t": "edit"`, and the authenticated data names the post being edited. Clients accept an edit only from the device's user who wrote the original.
- **Deletes** stay as they are (`DELETE /api/v4/posts/{id}`). The server deletes the ciphertext. Clients also delete their local copy.
- **Reactions:** encrypted events through `POST /api/v4/e2ee/channels/{id}/events`, plaintext `{"t":"reaction","post_id":"…","emoji":"…","on":true}`. The server stores them against the post and counts them, but can't see which emoji. Clients show the decrypted reactions. Plain `POST /reactions` is refused in encrypted channels.
- **Threads:** `root_id` stays visible to the server, so threads, reply counts and the Threads list keep working.
- **Pinning, saving and read state** are plain operations on post IDs and stay as they are.

As built (step 7):
- **Edits:** the edit's ciphertext goes in `props.dm_edit` beside the original `dm_e2ee`, which stays because it holds the file keys. Its authenticated data is `{kind: edit, channel_id, post_id}` with no id of its own. A server that shows an earlier edit again is only withholding the later one, which it can always do. A device keeps each edit's text under the post and the edit's ciphertext hash.
- **Events:** `{post_id, client_id, dm_e2ee}`. The authenticated data names the post and the sender's `client_id`, and the reaction inside names the post again. They come with posts as `metadata.e2ee_events`, oldest first, and live as the `e2ee_event` websocket event. A retry with the same `client_id` gets the stored event. Each person's last event per emoji decides whether their reaction shows.
- **Deletes:** the server removes `dm_e2ee`, `dm_edit` and the post's events. Devices forget the post's text, edits, events and pending copy.

## 9. Mentions, unreads and notifications

The server can't read a message, so it can't find @mentions in it. The sender's client does, and lists the mentioned user IDs in `props.dm_mentions` (`@channel`, `@all` and `@here` become a flag). The server uses that for mention counts and notifications.

- **The trade-off:** the server learns who was mentioned, but not what was said. In DMs every message counts as a mention anyway. This is in the metadata list in [§1](#1-goals-and-limits). (Decided, [§18](#18-decisions).)
- A client can't lie its way into someone else's mention count beyond what any channel member could do by mentioning them.
- **As built:** `dm_mentions` holds user ids, and `@channel`, `@all` or `@here`. The server counts only channel members, never the author, and not someone who blocked the author.
- **Unread counts** work as today: they count posts, not content.
- **Push notifications** on phones (M4, on hold) carry no content: the phone wakes, fetches the post by ID, decrypts it and shows the text itself.

## 10. Search

The server's search (FTS5) never sees encrypted content and skips encrypted channels. Each client keeps a **local search index** of what it has decrypted, in its encrypted local database ([§13](#13-storage-on-the-client)). Searching shows server results for plain channels and local results for encrypted ones, merged.

## 11. Calls

As built in M3 Phase 2, with SFrame since Phase 6 ("Phase B" of [research/calls.md](research/calls.md) §4), beta:
- **Each call has its own MLS group** (`call:<call id>`), holding only the devices in the call, with the same device credentials as chat, so chat verification carries over and people in the channel but not the call derive nothing.
  - The first device in the call creates the group. Each later one **joins by itself with an external commit** from the group's latest GroupInfo, so nobody adds anyone.
  - axiond, as the group's external sender, **proposes the Remove** of a device that leaves the call, is signed out or drops; a member still in the call commits it (each waits a moment of its own first, so most find it done).
  - An external commit can't carry pending proposals (RFC 9420 §12.4.3.2), so for a call's group axiond lets a join through without them and proposes them again in the new epoch.
  - The server keeps each group's public state in memory with the call, and checks every commit with the same delivery service as channels. It refuses commits that add anyone, external joins by a device other than the sender or not in the call, and removals of a device still in the call.
  - Every join and leave is a new epoch.
- **Frame encryption: SFrame ([RFC 9605](https://www.rfc-editor.org/rfc/rfc9605)) in its MLS mode (§5.2),** through the `sframe` crate (ring's AES-GCM), cipher suite `AES_128_GCM_SHA256_128`.
  - Every epoch, `base_key = MLS-Exporter("SFrame 1.0 Base Key", "", 16)`. A frame's key id is `context << 12 | leaf_index << 4 | (epoch mod 16)`, where the context is the stream (0 sound, 1 camera, 2 screen). SFrame derives each key and salt from the base key and the key id, so every sender and stream has its own key and its own counter, and no key and nonce repeat.
  - Every member can derive every sender's key, and nobody outside the group can.
  - Receivers install an epoch's keys as soon as they process its commit and keep the last 4 epochs (an old epoch with the same low 4 bits is evicted first, as §5.2 says). A sender switches to its new key **one second later**, so the others have it first; a device just removed can read for that long.
  - **Nothing is sent before there's a key**, and a frame that doesn't authenticate under a key the device has, or that it has seen before (replay protection, with 256 frames of reordering allowed), is dropped.
  - Frames go through our own frame transformer in libwebrtc (`vendor/webrtc-sys`, see [vendor/README.md](../../vendor/README.md)), on every sender and receiver.
- **The call code:** eight digits from the epoch authenticator (RFC 9420 §8.7), the same for everyone in the call and new with every join or leave. Read out, it shows nobody's view of the call was split.
- **Never reuse a key and nonce:** each call is a fresh group and each epoch fresh keys, so no key outlives its epoch; within one, SFrame's nonce is the salt XOR a counter that only goes up, per sender and stream.
- Opus at a constant bitrate, no DTX, no audio-level header extension. Sound is encrypted whole.
- Video is VP8 only (the SFU offers nothing else). Its first 3 bytes, or 10 on a keyframe (keyframe flag, size), stay readable for RTP packetization and the SFU; they're SFrame's authenticated "metadata", so they can't be changed.
- The SFU forwards ciphertext it can't decode.

**Known limits:**
- No per-sender authentication of frames: any member could forge frames as another (RFC 9605 §7.2; signing frames costs too much).
- VP8's frame header stays readable: the server sees video size and keyframes.
- There's no standard RTP format for SFrame yet (`draft-ietf-avtcore-rtp-sframe` expired); ours is SFrame frames inside ordinary RTP payloads, which only Darkmatter needs to read.

## 12. New devices, verification and recovery

**Linking a new device** (as built in M2 step 9). The new device logs in, makes its MLS signature key and a one-off HPKE key, and sends both to the server as a link request. It shows a **link code**: 20 characters (100 bits) of a SHA-256 hash over the user id, device id and both keys, and a QR code of it. On one of the user's existing devices, Account takes the code, finds the pending request whose keys hash to it, and shows which app it is. "Link it" signs the new device's certificate and seals the identity key to its HPKE key (RFC 9180, X25519 / HKDF-SHA256 / AES-128-GCM, from the same libcrux provider as MLS), bound to the user and device ids. The new device checks the certificate vouches for its own key and that the identity key it got is the account's, then joins the user's groups by external commit. A server that swapped in keys of its own would change the code, so the existing device wouldn't find a match. (This replaces the one-off MLS group first planned: HPKE does the same job with less machinery.)

**Recovery** (as built in M2 step 10). Account → Recovery offers a **recovery key**: 128 random bits shown once as a 28-character code (26 characters for the key, 2 that catch typing mistakes; a code rather than words, so there's no word list to ship and translate). From it, HKDF-SHA256 with a random salt derives a backup key; a passphrase instead goes through argon2id (64 MiB, three passes; clients refuse settings below OWASP's floor, so a server can't hand out cheap ones). XChaCha20-Poly1305, bound to the user id, encrypts:
- the user identity key (so a lost-every-device user can sign a new device without breaking everyone's verification);
- later, an optional **history backup**: the device's decrypted messages, uploaded encrypted in chunks.

The server stores only the ciphertext. A passphrase can be used instead of a recovery key, stretched with argon2id with strong settings, but the app recommends the recovery key.

**With no recovery key and no device left,** the user makes a new identity key. Everyone sees the warning from [§3](#3-identities-and-devices), and the new devices see only messages from then on.

**Starting over** needs the account's password: the server takes out every device, drops the old backups, records it in the audit log and tells the user's teammates (`identity_changed`).

**Verification.**
- A **safety number** for each pair of users: 60 digits computed from both identity keys (the same idea as Signal's), compared in person or over a call.
- A **QR code** for in-person verification.
- Verified users show a mark. A changed identity key removes it and shows the warning.

## 13. Storage on the client

Encrypted chat needs Darkmatter to keep state on disk:
- the MLS state (openmls's key/value pairs, written after every change, deletions included);
- decrypted messages and the local search index;
- verification state and known identity keys.

**One local SQLite database per account** (the "vault", built in M2 step 3), encrypted at rest with **SQLCipher** under a random 256-bit key in the OS keychain (the `keyring` crate, as sessions use; a 0600 file when there's no keychain). The files are readable only by the user, `secure_delete` overwrites freed pages, and writes go through one ordered queue. It holds the outbox too, on Mattermost servers as well. SQLCipher uses the system's libcrypto on Linux and CommonCrypto on macOS. If its key is lost, the old file is set aside and the device starts again as a new device.

## 14. What a malicious server can and can't do

| Attack | Result |
|---|---|
| Read messages, files, reactions, call media | **Can't.** It has only ciphertext. |
| Add a device or user of its own to a group | **Can't** without a user identity key to sign a device certificate. Clients reject unsigned devices. It can't make Add proposals (Remove only). |
| Swap a user's identity key | **Detected.** Warning in every conversation; sending blocked where the user was verified. Unverified first contact is trust on first use, as in Signal. |
| Move a message to another channel or thread, or replay it | **Detected** through the authenticated data ([§6](#6-messages)). |
| Drop, delay or reorder messages | **Possible.** MLS detects gaps within an epoch, and clients show "some messages couldn't be delivered". Availability needs a server we run ourselves. |
| Withhold a commit from some members (fork the group) | **Detected** when the forks next meet: epochs and tree hashes won't match. Clients show a warning and recover (openmls fork resolution). |
| Keep a removed member's device | It can keep *delivering* to it, but the device can't decrypt anything after the next commit, which members make before sending. |
| See metadata | **Yes:** membership, timing, sizes, mentions, call participation ([§1](#1-goals-and-limits)). |
| Serve a modified Darkmatter | **Not via the server.** Darkmatter is a signed native app from our releases, unlike web apps delivered by the server. |

## 15. API

All new paths are under `/api/v4/e2ee/`; the existing posts, files and channels APIs carry the rest. Websocket events: `mls_commit`, `mls_welcome`, `e2ee_event`, `device_added`, `device_removed`, `identity_changed`.

| Endpoint | Purpose |
|---|---|
| `GET e2ee/config` | Server capabilities, ciphersuites, the server's external sender key. Darkmatter uses this to tell axiond from plain Mattermost. |
| `PUT e2ee/identity` | Publish the user identity public key (first device, or after a reset). |
| `GET e2ee/users/{id}/identity` | A user's identity key, device certificates and revocations. |
| `POST e2ee/devices` / `DELETE e2ee/devices/{id}` | Publish a device certificate; publish a signed revocation. |
| `POST e2ee/key_packages` | Upload key packages for this device. |
| `POST e2ee/key_packages/claim` | Claim one key package per device for a list of users. |
| `GET e2ee/channels/{id}/group_info` | The latest `GroupInfo`, for external commits. Members only. |
| `POST e2ee/channels/{id}/handshake` | Send a commit (with its Welcomes and the new `GroupInfo`). `409` if the epoch moved on. |
| `GET e2ee/channels/{id}/handshake?after=` | Fetch handshake messages after a sequence number. |
| `GET e2ee/welcomes` | Pending Welcomes for this device. |
| `POST e2ee/channels/{id}/events` | Encrypted reactions and other small events. |
| `PUT e2ee/backup` / `GET e2ee/backup` | The encrypted identity-key and history backup. |

## 16. Testing

From PLAN.md's [review section](PLAN.md#review-without-a-paid-audit-decided-2026-10-19), made specific:
- openmls's RFC 9420 test vectors run in our CI against the version we pin.
- **Property tests** over random sequences of adds, removes, updates, sends, and devices going offline and coming back, checking every device ends up in the same epoch and decrypts every message it should, and none it shouldn't.
- **A malicious-server test harness:** a test axiond that drops, reorders, replays, moves and forges messages and key material, checking each attack in [§14](#14-what-a-malicious-server-can-and-cant-do) is refused or detected.
- **Fuzzing** (`cargo-fuzz`) of everything that parses data from the network: `dm_e2ee`, the plaintext JSON, device certificates, backups, file chunks.
- **Cross-platform:** `dm-crypto` builds and its tests pass for `aarch64-apple-ios` and `aarch64-linux-android` in CI from the start.

## 17. What M1 builds because of this

Built now, so M2 doesn't redo it:
- **Device IDs on sessions** (the column exists since M0): every login gets one, and the sessions page shows them.
- **Files are stored as opaque blobs** under random IDs and served as downloads; thumbnails are optional per file, so encrypted files without server thumbnails just work.
- **Search skips channels flagged `encrypted`**, and the client merges results from more than one source.
- **Reactions and mentions** go through code paths that can take a different source later (`e2ee/events`, `dm_mentions`).
- **Darkmatter's local database** (SQLCipher, keychain key) arrives with the offline outbox, so M2 adds MLS tables rather than a new storage layer.
- **`GET e2ee/config`** answers with `{"enabled": false}`, so Darkmatter's server detection is in place.

## 18. Decisions

Decided with the project's admins on 2026-10-20:
1. **Identity key changes in verified conversations:** sending is blocked until the new key is accepted ([§3](#3-identities-and-devices)).
2. **Mentions:** the sender lists who was mentioned, so the server learns who, not what ([§9](#9-mentions-unreads-and-notifications)).
3. **History backup:** not in M2. New devices see messages from when they join; the encrypted history backup comes later ([§12](#12-new-devices-verification-and-recovery)).
4. **Recovery:** both a recovery key and a passphrase, with the app recommending the key ([§12](#12-new-devices-verification-and-recovery)).
