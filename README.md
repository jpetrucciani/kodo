# kodo

A small, fast WoW addon manager for Windows, macOS and Linux, built for WoW: Forever first. No ads, no account, no telemetry, nothing running in the background. Desktop app plus a CLI that does everything the app does.

## What it does

- Install, update, pin, roll back and remove addons from GitHub, WoWInterface and (with your own API key) CurseForge
- Picks up addons you already have and matches them to their sources
- Backs up addon folders and their saved settings before every change; anything can be undone from History
- Profiles: sets of addons you can switch between, or share with friends as a code
- Per-character addon switches, copy one character's setup to another
- Health checks: missing dependencies, out-of-date TOCs, duplicates, unused libraries
- Import a list from WowUp or plain text; install from a zip or a link

## Install

Grab the latest build from [Releases](https://github.com/jpetrucciani/kodo/releases):

| OS      | File                                                                            |
| ------- | ------------------------------------------------------------------------------- |
| Windows | `.msi` installer, or the standalone `kodo-app-x86_64-pc-windows-msvc.exe`       |
| macOS   | `kodo-aarch64-apple-darwin.dmg`, Apple silicon (unsigned: right-click → Open the first time) |
| Linux   | `kodo-x86_64.AppImage`, or the tarball |
| Nix     | `nix-build https://github.com/jpetrucciani/kodo/archive/main.tar.gz -A kodo`    |

Run `kodo-app` for the GUI and `kodo` for the CLI. A `portable.txt` next to the executable (or `--portable`) keeps all of kodo's data in a `kodo-data` folder beside it.

## CLI

```sh
kodo clients                    # WoW installs kodo found
kodo search bags
kodo install github:Cidan/BetterBags
kodo outdated                   # exit 1 when something has an update
kodo update --all
kodo rollback BetterBags
kodo profile switch raid
kodo --help                     # everything else
```

Most commands take `--json`.

## Building

```sh
nix-shell                       # or direnv; toolchain from rust-toolchain.toml
cargo run -p kodo-app
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Releases are automatic: bumping `version` in `Cargo.toml` on `main` tags and publishes it (a `-rc.N` version becomes a prerelease).

## License

MIT. Bundles Inter (OFL) and Lucide icons (ISC).
