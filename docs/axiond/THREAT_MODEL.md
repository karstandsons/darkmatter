# Threat model: Darkmatter with axiond

What end-to-end encryption in Darkmatter and axiond protects, from whom,
and what it doesn't. The design is [E2EE.md](E2EE.md); this is what it
amounts to, as built in M2. Encryption is **beta** until the review steps
in [PLAN.md](PLAN.md#review-without-a-paid-audit-decided-2026-10-19) are
done.

## What's protected

In an encrypted conversation (every direct message and private channel on
axiond, and public channels an admin turned it on for):
- the text of messages, edits and reactions;
- calls, in any channel: what's said (and, with video, shown), keyed from
  each call's own group, so only the devices in the call can hear it;
- files and pictures, their names, types and sizes, and their thumbnails;
- who was mentioned is sent beside the message, so the server can count
  mentions: it learns *who*, not *what*.

Only the devices of the people in the conversation at the time can read
them. MLS (RFC 9420, through openmls) gives each conversation its own
keys, changes them whenever someone joins or leaves, and every device
refreshes its own key weekly and after a day away.

## Who we defend against

| Adversary | Result |
|---|---|
| Someone on the network | Sees TLS only. |
| The server's operator, its host, or someone who took it over, reading | Gets ciphertext. They can't decrypt messages, files, edits or reactions. |
| The same, *acting*: adding devices, swapping keys, moving or replaying messages | See [A malicious server](#a-malicious-server). |
| A stolen copy of the server's database or backups | Ciphertext, argon2id password hashes, hashed session tokens, and the identity key backups, which need the recovery key or passphrase. |
| A stolen copy of a device's files | Darkmatter's local database (decrypted messages, MLS state, search index) is SQLCipher, under a key in the system keychain. A copy of the files alone is unreadable. |
| Someone removed from a conversation | Reads nothing sent after: they're removed from the group before anything else is sent. The server refuses messages for the epoch their removal was proposed in, or any before it, so one encrypted a moment before they left (or sent late) is refused, and the sender's app commits the removal and sends it again. |
| Someone who left a call | Hears at most the next second: a new epoch begins as they leave, and senders switch to its keys a second later. |
| A plugin in Darkmatter | Gets no encrypted text unless you tick "May read encrypted conversations" for it. |
| Someone signed in (or, for connections and call media, anyone) trying to run the server out of memory or disk | Held to limits: upload quotas and rates, a budget for decoding pictures, caps on websockets, connections and slow clients, bounded call-media queues. See [Limits](../../crates/axiond/README.md#limits). A determined flood can still slow the server for everyone. |
| Someone using up another's one-time key packages | Can't: only people who share a channel can claim them, and past a few, they get the last-resort package. |

## A malicious server

What a server that turned on its users can and can't do, and how the
clients catch it. Each row is a test in
`crates/axiond/tests/malicious_server.rs` or `crates/dm-crypto/tests/`.

| It tries to | Result |
|---|---|
| Read anything encrypted | Can't. |
| Add a device of its own to someone's account | Can't: devices need a certificate signed by the user's identity key, which never leaves their devices (or their encrypted backup). Clients leave out devices whose certificate doesn't verify, and keep talking to the real ones. |
| Add a device of its own when someone links a new one | Can't: the link code is a hash of the new device's keys, so swapped keys don't match it. A forged approval is refused by the new device. |
| Swap someone's identity key | Noticed. If you verified them (`/verify`), nothing is sent where they are until you accept the change. If you didn't, the change is accepted and the conversation says so, as Signal does. |
| Move a message to another conversation, thread or sender, or replay it | Not shown: the authenticated data and the MLS sender don't match, and a message's keys work once. |
| Swap or weaken a backup | A backup is bound to its user, and clients refuse argon2id settings below OWASP's floor. |
| Drop, delay or reorder messages | Possible. Within an epoch MLS shows gaps; the server can always refuse service. |
| Add a device to a call, or split it | Can't add one: devices join a call's group by themselves, with certificates the server can't make. A split shows: everyone's call code is the same only if they share one group. |
| Keep a removed member in the group, by dropping its removal | Can't: before sending, apps take out the devices of anyone no longer in the channel themselves. |
| Withhold a commit from some members | They fall out of step; a device that can't apply a handshake rejoins the group. |
| Learn who talks to whom, when, how much | Yes: see [What isn't protected](#what-isnt-protected). |

## What isn't protected

- **Metadata.** The server sees who is in which conversation, when
  messages are sent and how big they are (padded to 256 bytes), who
  reacted to which message (not with what), who was mentioned, and file
  sizes.
- **Conversations that aren't encrypted.** Public channels on axiond are
  readable by the server unless an admin turns encryption on; on
  Mattermost nothing is end-to-end encrypted. Darkmatter shows 🔒 on the
  ones that are.
- **First contact.** Until you compare safety numbers, you trust the key
  the server gave you the first time (trust on first use).
- **An unlocked device.** Whoever holds it reads what it can. Signing it
  out from another device takes it out of every conversation for what's
  sent next; what it already decrypted stays on it.
- **Old messages on a new device.** A linked or recovered device reads
  only what's sent from then on. (An encrypted history backup comes
  later.)
- **Calls' metadata.** Who is in a call, from which device and address,
  when and for how long, and packet sizes and rates. Audio is sent at a
  constant bitrate without pauses in silences, and without the header that
  says how loud it is, so packet sizes and gaps don't give away speech.
- **Within a call, who sent a frame.** Every device in a call can derive
  every sender's key, so one could forge another's frames (RFC 9605
  §7.2). Calls use SFrame, so frames can't be replayed or moved between
  senders or streams, but not signed.
- **Notifications.** Desktop notifications from encrypted conversations
  say only "🔒 New message" unless you turn previews on, since the
  desktop keeps their text.
- **Copying out.** Text copied from an encrypted conversation and pasted
  elsewhere is out of our hands; Darkmatter asks before sending it into a
  conversation that isn't encrypted.
- **Revocations aren't signed.** Taking a device out is the server's job,
  so a malicious server could cut one off, but it could refuse it service
  anyway.

## Assumptions

- The operating system and the Darkmatter binary are trusted: releases are
  built in CI and signed; a compromised machine is out of scope.
- The system keychain protects the local database key as well as it
  protects the user's other secrets.
- openmls, libcrux and the RustCrypto crates are correct. Our own code adds
  no cryptographic primitives (see E2EE.md).

## How it's checked

- Unit and end-to-end tests through a real axiond
  (`crates/axiond/tests/encrypted_dms.rs`).
- The malicious-server harness (above).
- Property tests: random runs of adds, removals, key updates and messages,
  out of order, checking everyone stays in step
  (`crates/dm-crypto/tests/properties.rs`).
- Fuzzing every parser that reads what the server sends
  (`crates/dm-crypto/tests/fuzz.rs`).
- Community review: the code is open; please report problems privately to
  the maintainers rather than in a public issue.
