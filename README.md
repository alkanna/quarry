<p align="center"><img src="assets/icon-256.png" width="96" alt="Quarry icon"></p>

<h1 align="center">Quarry</h1>

<p align="center">A fast, stable and full-featured MongoDB client, written in Rust with egui.<br>Inspired by <a href="https://mingo.io">Mingo</a>.</p>

![Documents view](docs/documents.png)

## Features

- **Browse and query**: tabs, table or JSON view, pinned and reordered columns per collection, sort from the header, query history and a saved query library. Paste `db.orders.find(...)` or `aggregate([...])` straight into the query bar. Editors auto-close brackets and indent as you type. Date shortcuts such as `#today`, `#yesterday` or `#last2months` expand to date filters and show up in autocomplete.
- **Jump anywhere** with the Cmd+K palette, and drive tabs, views, reload and paging from the keyboard (Cmd+/ lists the shortcuts; Cmd is Ctrl on Linux and Windows).
- **Safe editing**: every connection opens read-only. Edits are typed, staged and saved together, never overwrite a change someone else made in the meantime, and can be undone. Update every matching document after previewing the result.
- **Aggregate** with a preview of the output after each stage, and **explain plans** for queries and pipelines.
- **Copy as driver code**: turn a query or pipeline into code for the official MongoDB drivers.
- **Indexes**: usage stats, hide and unhide, and suggestions from slow queries.
- **Schema**: field types and presence, plus viewing, generating and applying collection validators.
- **Import**, and export to JSON, CSV or TSV.
- **Query log**: every command sent to the server, with its duration and errors, docked at the bottom or in its own window (Cmd+L).
- **Shell**: JavaScript run by [Bun](https://bun.sh) with the official MongoDB driver, npm packages, autocomplete, and results shown as a table.
- **Data relations**: shows a preview next to referenced ids and jumps to the referenced document. Relations can be found automatically.
- **Compare and sync** two collections, **dump and restore** with the MongoDB Database Tools (Quarry installs and updates them if they are missing), and **users and roles**.
- **SSH tunnels** (password, key file or agent) and **SOCKS5 proxies**. Secrets are kept in the system keychain (macOS Keychain, Windows Credential Manager, or the Secret Service on Linux). Quarry reconnects to the last used connection at launch.

| | |
|---|---|
| ![Editing a document](docs/edit.png) | ![Shell](docs/shell.png) |
| ![Schema](docs/schema.png) | ![Compare and sync](docs/compare.png) |

## Install

Download the latest build for your system from [Releases](https://github.com/alkanna/quarry/releases).

**macOS** (Apple silicon, Intel, or universal): unzip `Quarry-macos-*.zip` into Applications. The app is not notarized, so run `xattr -dr com.apple.quarantine /Applications/Quarry.app` once.

**Linux** (x86_64 and ARM64): extract `quarry-linux-*.tar.gz` and put the `quarry` binary somewhere on your `PATH`, such as `~/.local/bin`. It needs GTK 3 and xkbcommon (`sudo apt install libgtk-3-0 libxkbcommon0` on Debian and Ubuntu), and a Secret Service provider such as GNOME Keyring or KWallet to store passwords.

**Windows** (x86_64 and ARM64): unzip `quarry-windows-*.zip` and run `quarry.exe`. The binary is not signed, so SmartScreen may warn the first time: choose More info, then Run anyway.

Quarry then updates itself: it checks for new releases shortly after launch and once a day, installs them in the background, and restarts once you have been idle for a minute with nothing unsaved. It shows what's new after an update. You can turn updates off in Settings.

## Build

Needs Rust (stable). The Shell needs [Bun](https://bun.sh), and dump/restore needs the [MongoDB Database Tools](https://www.mongodb.com/docs/database-tools/), which Quarry can install for you. Quarry finds both automatically, or you can set their paths in Settings.

```sh
cargo run --release      # run it
./bundle.sh              # package for this machine into dist/
./bundle.sh linux-x86_64 # or for a given platform (./bundle.sh --help lists them)
cargo test               # run the tests
```

`quarry --screenshot out.png` draws the offline demo, saves a picture and exits. It never connects to a database.

## Releasing

Commits to `master` follow [Conventional Commits](https://www.conventionalcommits.org) (`feat:`, `fix:`, ...). [release-please](https://github.com/googleapis/release-please) keeps a release PR open; merging it tags the version and the release workflow attaches the builds. In the repository settings, under Actions > General, allow GitHub Actions to create pull requests.

## License

MIT, see [LICENSE](LICENSE).
