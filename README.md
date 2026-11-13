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

Quarry works with MongoDB, MySQL 5.7 to 9, MariaDB 10.6 and later, and PostgreSQL 13 and later. Connections to different engines open side by side in tabs and share the same shortcuts, editing model and admin windows.

It is free and open source under the [MIT license](LICENSE). There is no paid edition.

## Working on production

Every connection opens read-only until you unlock it. Once unlocked, edits are staged as drafts that you review, save together and can undo. A save does not overwrite a change someone else made to the same record in the meantime.

Read-only mode covers what Quarry sends. A server-side function that writes on its own still runs with your role's rights, so use a read-only role where that matters.

## Features

**Queries**

- MongoDB `find` and `aggregate`, with a preview after each pipeline stage, explain plans and copy as code.
- SQL through typed filter rules or a full editor that runs several statements in one go, with autocomplete, Format, Explain and Stop.
- A script library for every engine, with folders and Cmd+S to save. A folder can be linked to a directory on disk, so a team keeps its `.sql`, `.js` and `.json` scripts in a repo.
- History and a log of every query sent.
- Schema analysis of the documents a query matches, group and count on any field, and change streams.

**Scripts**

- A JavaScript shell that runs on [Bun](https://bun.sh), with completion and diagnostics from a language server.
- One script can use several connections, so it can join rows from PostgreSQL with documents from MongoDB in the same run.

**Structure and data**

- Editors for indexes, columns, foreign keys, triggers and MongoDB validators.
- A relations view of the foreign keys between tables.
- Import and export of JSON, CSV, TSV and SQL dumps.
- Compare and sync between collections, and MongoDB dump and restore.

**Administration**

- Server status, settings and running sessions, with Kill or Cancel.
- Users, roles and privileges, including the PostgreSQL object privilege matrix and lock view.

**Connections**

- SSH tunnels, SOCKS5 and TLS, with client certificates on PostgreSQL. PgBouncer in transaction mode is supported.
- Google Cloud SQL: pick an instance from your gcloud account and Quarry runs the Cloud SQL Auth Proxy for you.
- Colored connection groups in the sidebar, folded into one tile each in the rail. Drag connections in and out of them.
- Passwords are stored in the system keychain.

**Interface**

- Cmd+K searches every connection, saved script and command. Cmd+/ lists every shortcut.
- Drag to reorder grid columns, tabs, pipeline stages and table columns.
- 17 themes, larger text, OpenDyslexic, a soft background and a reading line for wide tables.

| | |
|---|---|
| ![SQL Query with several statements](docs/sql-query.png) | ![Aggregation pipeline with a preview after each stage](docs/aggregate.png) |
| ![A shell script reading a SQL and a MongoDB connection](docs/shell-attach.png) | ![PostgreSQL roles and privileges](docs/pg-roles.png) |

## Performance

Quarry is a single Rust binary, about 25 MB to download. It draws its own window on the GPU with [egui](https://github.com/emilk/egui) and uses neither Electron nor a web view. Database work runs off the UI thread, so the window keeps responding while a slow query is out. Result pages are kept as raw BSON, which takes about a tenth of the memory of decoded documents.

Measured with `bench.py` on an M1 Pro, with a full page of 1,000 rows open:

| | MongoDB documents | SQL rows |
|---|---|---|
| Memory footprint | 46 MB | 50 MB |
| CPU when idle | 0.1 % | 0 % |
| Frame time, median | 0.9 ms | 2.1 ms |
| Frame time, 95th percentile | 3.0 ms | 4.1 ms |

With nothing open, the footprint is 33 MB. Footprint is the figure Activity Monitor shows; RSS is higher.

## AI agents

Quarry includes an [MCP](https://modelcontextprotocol.io) server, so agents such as Claude Code, Claude Desktop, Cursor and Codex can query and change your databases through it. It is off until you turn it on in Settings > AI agents, which also shows the command that adds Quarry to each client. For Claude Code on macOS:

```sh
claude mcp add --scope user quarry -- /Applications/Quarry.app/Contents/MacOS/quarry mcp
```

The agent works through the open Quarry window, so Quarry has to be running. What an agent can reach is set per connection:

- A connection is not shared until you share it, for reading or for reading and writing. An agent's writes also need the connection unlocked, like yours.
- For each connection, reading, changing data, changing structure and running scripts are each set to Never, Ask or Allow. Ask shows the call in Quarry, with what it would touch, before it runs. Structure changes and shell scripts always ask.
- Secrets and personal data are scrubbed before anything leaves Quarry. Secrets are hidden everywhere. Personal data can still be queried but comes back masked. Your own rules add field paths, field names, regexes and value detectors.
- Changes made by an agent can be undone from the Activity window, within a size limit, and every call is kept in an audit log on disk.

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

Commits to `master` follow [Conventional Commits](https://www.conventionalcommits.org), and [release-please](https://github.com/googleapis/release-please) cuts releases from them. [CLAUDE.md](CLAUDE.md) describes the architecture and the UI conventions.

## License

MIT, see [LICENSE](LICENSE). Quarry was inspired by [Mingo](https://mingo.io).
