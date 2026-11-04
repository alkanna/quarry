<p align="center"><img src="assets/icon-256.png" width="96" alt="Quarry icon"></p>

<h1 align="center">Quarry</h1>

<p align="center">A fast, native MongoDB workbench for macOS, written in Rust with egui.<br>Built to read and edit production data safely.</p>

![Documents view](docs/documents.png)

## Features

- **Browse and query**: tabs, table or JSON view, pinned and reordered columns per collection, sort from the header, query history and a saved query library. Paste `db.orders.find(...)` or `aggregate([...])` straight into the query bar.
- **Safe editing**: every connection opens read-only. Edits are typed, staged and saved together, never overwrite a change someone else made in the meantime, and can be undone. Destructive actions ask you to type the name.
- **Aggregate, Indexes, Schema and Import** views, plus export to JSON, CSV or TSV.
- **Shell**: JavaScript run by [Bun](https://bun.sh) with the official MongoDB driver, npm packages, autocomplete, and results shown as a table.
- **Data relations**: shows a preview next to referenced ids and jumps to the referenced document. Relations can be found automatically.
- **Compare and sync** two collections, **dump and restore** with the MongoDB Database Tools, and **users and roles**.
- **SSH tunnels** (password, key file or agent) and **SOCKS5 proxies**. Secrets are kept in the macOS Keychain.

| | |
|---|---|
| ![Editing a document](docs/edit.png) | ![Shell](docs/shell.png) |
| ![Schema](docs/schema.png) | ![Compare and sync](docs/compare.png) |

## Install

Download the latest build for your system from [Releases](https://github.com/alkanna/quarry/releases): macOS (Apple silicon, Intel, or universal), Linux and Windows (x86_64 and ARM64).

The macOS app is not notarized, so after unzipping it into Applications, run `xattr -dr com.apple.quarantine /Applications/Quarry.app` once.

Quarry then updates itself: it checks for new releases shortly after launch and once a day, installs them in the background, and restarts once you have been idle for a minute with nothing unsaved. You can turn this off in Settings.

## Build

Needs Rust (stable). The Shell needs [Bun](https://bun.sh), and dump/restore needs the [MongoDB Database Tools](https://www.mongodb.com/docs/database-tools/) (`brew install mongodb-database-tools`). Quarry finds both automatically, or you can set their paths in Settings.

```sh
cargo run --release      # run it
./bundle.sh              # build dist/Quarry.app
cargo test               # run the tests
```

`quarry --screenshot out.png` draws the offline demo, saves a picture and exits. It never connects to a database.

## Releasing

Commits to `master` follow [Conventional Commits](https://www.conventionalcommits.org) (`feat:`, `fix:`, ...). [release-please](https://github.com/googleapis/release-please) keeps a release PR open; merging it tags the version and the release workflow attaches the builds. In the repository settings, under Actions > General, allow GitHub Actions to create pull requests.

## License

MIT, see [LICENSE](LICENSE).
