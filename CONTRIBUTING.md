# Contributing

Thanks for wanting to help. Darkmatter is open source and maintained by
Karst & Sons, who decide what goes in and where the project heads.

- **Open an issue first** for anything bigger than a small fix, so we can agree
  on the approach before you spend time on it.
- **Licensing:** what you contribute is licensed like the rest of the project,
  under MIT or Apache-2.0 (Apache-2.0 section 5), and you keep your copyright.
  Please only send code you wrote or have the right to share.
- **Before a pull request:** `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` must pass (CI checks both, on Linux and macOS).
  Keep the code in the style around it.
- **Testing UI changes:** `dev/seed.sh` starts a local Mattermost with test
  users. Never test write actions against a real server; `--read-only` exists
  for looking around one.
- **The name:** forks are welcome under their own name. See [TRADEMARKS.md](TRADEMARKS.md).
