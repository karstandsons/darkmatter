# Contributing

Thanks for wanting to help. Darkmatter is open source and maintained by
Karst & Sons, who decide what goes in and where the project heads.

- **Open an issue first** for anything bigger than a small fix, so we can agree
  on the approach before you spend time on it.
- **Licensing:** what you contribute is licensed like the rest of the project,
  under MIT or Apache-2.0 (Apache-2.0 section 5), and you keep your copyright.
  The exception is the server, `crates/axiond`: contributions there are under
  AGPL-3.0-or-later, like the rest of it.
  Please only send code you wrote or have the right to share.
- **Before a pull request:** `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` must pass (CI checks both, on Linux and macOS).
  Run `dev/fetch-webrtc.sh` once first: calls (`crates/dm-call`) link a
  prebuilt libwebrtc, which it downloads and checks against its published
  SHA-256. On Linux that build also needs glib's headers (`libglib2.0-dev`).
  Keep the code in the style around it.
- **Testing UI changes:** `dev/seed.sh` starts a local Mattermost with test
  users. Never test write actions against a real server; `--read-only` exists
  for looking around one.
- **The name:** forks are welcome under their own name. See [TRADEMARKS.md](TRADEMARKS.md).
