<p align="center"><img src="assets/icon-256.png" width="96" alt="Quarry icon"></p>

<h1 align="center">Quarry</h1>

<p align="center">A fast, stable and full-featured client for MongoDB, MySQL, MariaDB and PostgreSQL, written in Rust with egui.<br>Inspired by <a href="https://mingo.io">Mingo</a>.</p>

<p align="center">
  <a href="https://github.com/alkanna/quarry/releases/latest"><img src="https://img.shields.io/github/v/release/alkanna/quarry" alt="Latest release"></a>
  <a href="https://github.com/alkanna/quarry/releases"><img src="https://img.shields.io/github/downloads/alkanna/quarry/total" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-informational" alt="macOS, Linux and Windows">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/alkanna/quarry" alt="MIT license"></a>
</p>

![Documents view](docs/documents.png)

## Features

- **MongoDB, MySQL, MariaDB and PostgreSQL** side by side (MySQL 5.7, 8 and 9, MariaDB 10.6+, PostgreSQL 13+), in tabs with a table or JSON view. PostgreSQL databases show their schemas in a picker in the sidebar.
- **Safe editing.** Every connection opens read-only until you unlock it. Edits are staged, saved together and can be undone, and never overwrite a change someone else made meanwhile. Bulk updates, structure changes and drops show what they will run first. On PostgreSQL every statement of a locked connection runs in a read-only transaction that is rolled back; a function on the server that writes by itself still runs with your role's rights, so use a read-only role where that matters.
- **Queries.** Paste `db.orders.find(...)` or `aggregate([...])`, use date shortcuts like `#today`, build a pipeline with a preview after each stage, read explain plans and copy as driver code. For SQL, filter with typed rules or plain SQL, or run several statements at once with autocomplete, Format, Explain and Stop. Saved queries, history and a query log (Cmd+L).
- **Scripting.** A JavaScript shell on [Bun](https://bun.sh) with npm packages and autocomplete: the MongoDB driver, `` sql`...` `` templates, and other connections attached to the same script.
- **Structure.** MongoDB indexes with usage and suggestions, field types, validators, references and live change streams. SQL columns, indexes, foreign keys and triggers through a previewed ALTER. On PostgreSQL each change runs in one transaction that checks nobody changed the table since, and undo puts back exactly what it can or says what it cannot.
- **Import and export.** JSON, CSV, TSV and SQL dumps, on every engine, compare and sync two collections, dump and restore with the MongoDB Database Tools (installed for you).
- **Server.** Status, variables, running operations with Kill, users, roles and privileges (MongoDB, MySQL and MariaDB).
- **Connections.** SSH tunnels, SOCKS5 proxies and TLS. Paste a `postgresql://` string to fill the form. Passwords stay in the system keychain. Tag connections as Production, Staging or anything you like.
- **Keyboard and reading.** Cmd+K searches every connection and runs commands, Cmd+[ and Cmd+] move through visited tabs, Cmd+/ lists every shortcut (Ctrl on Linux and Windows). Reading options: larger text, the OpenDyslexic font, a soft background, a line that follows the pointer across wide tables, and grouped digits.

| | |
|---|---|
| ![A MySQL table filtered with rules](docs/sql-content.png) | ![SQL Query with several statements](docs/sql-query.png) |
| ![Structure with staged column and index changes](docs/sql-structure.png) | ![A SQL shell reading a MongoDB connection in the same script](docs/shell-attach.png) |
| ![Editing a document](docs/edit.png) | ![Aggregation pipeline with a preview after each stage](docs/aggregate.png) |
| ![Schema](docs/schema.png) | ![Compare and sync](docs/compare.png) |
| ![Server processes](docs/sql-server.png) | ![Connection with a tag and an SSH tunnel](docs/connection.png) |

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

Quarry needs GTK 3, xkbcommon, Wayland or X11, and a keyring such as GNOME Keyring or KWallet to store passwords. Most desktops have them; on Debian and Ubuntu, `sudo apt install libgtk-3-0 libxkbcommon0` covers what is usually missing.

<details>
<summary>NixOS</summary>

Add `wayland`, `libxkbcommon`, `libGL`, `vulkan-loader` and `gtk3` to `programs.nix-ld.libraries` (or set `LD_LIBRARY_PATH`).
</details>

**Windows.** Unzip and run `quarry.exe`. The binary is not signed, so SmartScreen may warn the first time: choose **More info**, then **Run anyway**.

Quarry keeps itself up to date: it installs new releases in the background and restarts once you have been idle for a minute with nothing unsaved. You can turn this off in Settings.

## Build

Needs Rust (stable). The Shell needs [Bun](https://bun.sh) and MongoDB dump and restore need the [Database Tools](https://www.mongodb.com/docs/database-tools/). Quarry finds both, or you can set their paths in Settings.

```sh
cargo run --release                              # run it
cargo test                                       # run the tests
./bundle.sh                                      # package for this machine into dist/ (--help lists other targets)
./target/release/quarry --screenshot out.png     # draw the offline demo to a picture and exit
```

`make` lists shortcuts: `make demo` opens the offline demo, and `make test-live` starts throwaway MongoDB, MySQL, MariaDB, PostgreSQL and PgBouncer servers in Docker and runs every test against them, including the headless UI tests.

On Linux and Windows, `bundle.sh` makes a release archive. To run your own build, take the binary from `target/release/`.

<details>
<summary>Stop the Keychain prompts on macOS</summary>

Each build gets a new ad-hoc signature, so macOS asks for your password again for every saved connection. `cargo run` signs with your Developer ID Application certificate if you have one, or else with a certificate named `Quarry Dev` (see `.cargo/run-signed.sh`), which keeps the signature stable. To create `Quarry Dev` once:

1. In Keychain Access, choose **Keychain Access > Certificate Assistant > Create a Certificate**.
2. Name it `Quarry Dev`, set Identity Type to **Self Signed Root** and Certificate Type to **Code Signing**, and click Create.
3. Run `cargo run` and click **Always Allow** at each prompt. Later builds won't ask again.
</details>

## Releasing

Commits to `master` follow [Conventional Commits](https://www.conventionalcommits.org). [release-please](https://github.com/googleapis/release-please) keeps a release PR open; merging it tags the version and the release workflow attaches the builds. GitHub Actions needs permission to create pull requests (Settings > Actions > General).

## License

MIT, see [LICENSE](LICENSE).
