<p align="center"><img src="assets/icon-256.png" width="96" alt="Quarry icon"></p>

<h1 align="center">Quarry</h1>

<p align="center">A native desktop client for MongoDB, MySQL, MariaDB and PostgreSQL.</p>

<p align="center">
  <a href="https://github.com/alkanna/quarry/releases/latest"><img src="https://img.shields.io/github/v/release/alkanna/quarry" alt="Latest release"></a>
  <a href="https://github.com/alkanna/quarry/releases"><img src="https://img.shields.io/github/downloads/alkanna/quarry/total" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-informational" alt="macOS, Linux and Windows">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/alkanna/quarry" alt="MIT license"></a>
</p>

![Documents view](docs/documents.png)

## Why Quarry

**It is native.** Quarry is one Rust binary, a download of about 25 MB, drawn on the GPU with [egui](https://github.com/emilk/egui). There is no Electron and no web view. Database work never runs on the UI thread, so the window stays responsive while a slow query is out, and result pages are held as raw BSON, which takes about a tenth of the memory of decoded documents.

Measured on an M1 Pro with `bench.py`, on a full page of 1,000 rows:

| | MongoDB documents | SQL rows |
|---|---|---|
| Memory footprint | 46 MB | 50 MB |
| CPU when idle | 0.1 % | 0 % |
| Frame time, median | 0.9 ms | 2.1 ms |
| Frame time, 95th percentile | 3.0 ms | 4.1 ms |

With nothing open, the footprint is 33 MB.

**It is hard to break production with.** Every connection opens read-only until you unlock it. Edits are staged as drafts, previewed, saved together and can be undone, and a save never overwrites a change someone else made in the meantime.

**It is one tool for four engines.** MongoDB, MySQL (5.7 to 9), MariaDB (10.6+) and PostgreSQL (13+) open side by side in tabs, with the same shortcuts, the same editing model and the same admin windows. One shell script can read from several of them at once.

## Features

- Query MongoDB with `find` and `aggregate`, with a preview after each pipeline stage, explain plans and copy as code.
- Query SQL with typed filter rules or a full editor: several statements at once, autocomplete, Format, Explain and Stop.
- Keep saved queries, history and a query log.
- Script in JavaScript on [Bun](https://bun.sh), across Mongo and SQL connections in the same script.
- Edit structure: indexes, columns, foreign keys, triggers and validators.
- Administer servers: status, sessions with Kill or Cancel, users, roles and privileges on every engine.
- Import and export JSON, CSV, TSV and SQL dumps, compare and sync collections, and run MongoDB dump and restore.
- Connect through SSH tunnels, SOCKS5 and TLS, with client certificates on PostgreSQL. Passwords stay in the system keychain.
- Pick a Google Cloud SQL instance from your gcloud account, and Quarry runs the Cloud SQL Auth Proxy for you.
- Work from the keyboard: Cmd+K searches every connection and runs commands, Cmd+/ lists every shortcut.
- Read comfortably: 17 themes, larger text, OpenDyslexic, a soft background and a reading line for wide tables.

Read-only mode blocks what Quarry sends. A server function that writes by itself still runs with your role's rights, so use a read-only role where that matters.

| | |
|---|---|
| ![SQL Query with several statements](docs/sql-query.png) | ![Aggregation pipeline with a preview after each stage](docs/aggregate.png) |
| ![A shell script reading a SQL and a MongoDB connection](docs/shell-attach.png) | ![PostgreSQL roles and privileges](docs/pg-roles.png) |

## Install

Download the file for your system from the [latest release](https://github.com/alkanna/quarry/releases/latest):

| System | File |
|---|---|
| macOS, Apple silicon | `Quarry-macos-arm64.zip` |
| macOS, Intel | `Quarry-macos-x86_64.zip` |
| Linux x86_64 | `quarry-linux-x86_64.tar.gz` |
| Linux ARM64 | `quarry-linux-aarch64.tar.gz` |
| Windows x86_64 | `quarry-windows-x86_64.zip` |
| Windows ARM64 | `quarry-windows-aarch64.zip` |

**macOS.** Unzip and drag `Quarry.app` into Applications. The app is signed and notarized.

**Linux.** Extract the binary somewhere on your `PATH`:

```sh
tar -xzf quarry-linux-x86_64.tar.gz -C ~/.local/bin
quarry
```

Quarry needs GTK 3, xkbcommon, Wayland or X11, and a keyring such as GNOME Keyring or KWallet. Most desktops have them. On Debian and Ubuntu, `sudo apt install libgtk-3-0 libxkbcommon0` covers what is usually missing. On NixOS, add `wayland`, `libxkbcommon`, `libGL`, `vulkan-loader` and `gtk3` to `programs.nix-ld.libraries`.

**Windows.** Unzip and run `quarry.exe`. The binary is not signed, so SmartScreen may warn the first time: choose **More info**, then **Run anyway**.

**Updates.** Quarry installs new releases in the background and restarts once you have been idle for a minute with nothing unsaved. A release is installed only if it matches its published checksum. You can turn updates off in Settings.

## Build from source

You need stable Rust. The Shell also needs [Bun](https://bun.sh), and MongoDB dump and restore need the [Database Tools](https://www.mongodb.com/docs/database-tools/). Quarry finds both, or you can set their paths in Settings.

```sh
cargo run --release    # run it
cargo test             # run the tests
./bundle.sh            # package for this machine into dist/
make demo              # open the offline demo, no server needed
```

The suite has about 1,200 tests. `make test-live` starts throwaway MongoDB, MySQL, MariaDB, PostgreSQL and PgBouncer servers in Docker and runs all of them, including headless UI tests that click through the real app and check the server afterwards.

<details>
<summary>Stop the Keychain prompts on macOS</summary>

Each build gets a new ad-hoc signature, so macOS asks for your password again for every saved connection. `cargo run` signs with your Developer ID Application certificate if you have one, or else with a certificate named `Quarry Dev` (see `.cargo/run-signed.sh`), which keeps the signature stable. To create `Quarry Dev` once:

1. In Keychain Access, choose **Keychain Access > Certificate Assistant > Create a Certificate**.
2. Name it `Quarry Dev`, set Identity Type to **Self Signed Root** and Certificate Type to **Code Signing**, and click Create.
3. Run `cargo run` and click **Always Allow** at each prompt. Later builds won't ask again.
</details>

## Contributing

Issues and pull requests are welcome. Commits to `master` follow [Conventional Commits](https://www.conventionalcommits.org), and [release-please](https://github.com/googleapis/release-please) cuts releases from them. [CLAUDE.md](CLAUDE.md) describes the architecture and the UI conventions.

## License

MIT, see [LICENSE](LICENSE). Quarry was inspired by [Mingo](https://mingo.io).
