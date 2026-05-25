# RySQL

A desktop GUI client for **MySQL** and **MariaDB**, written in Rust.

[![CI](https://github.com/arongarcia/rysql/actions/workflows/ci.yml/badge.svg)](https://github.com/arongarcia/rysql/actions/workflows/ci.yml)
[![Release](https://github.com/arongarcia/rysql/actions/workflows/release.yml/badge.svg)](https://github.com/arongarcia/rysql/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue)](#license)

> *Status: 0.6.0 — feature complete for an MVP daily-driver. Packaging and
> distribution channels are in place; awaits user feedback before 1.0.*

## Features

- **Connection manager** — host/port/socket, TLS modes, passwords stored in
  the OS keyring (Secret Service / Windows Credential Vault / Keychain).
- **Schema browser** — lazy tree of databases → tables / views / procedures
  / functions / triggers / events with copy-name, copy-CREATE and
  destructive actions guarded by a type-to-confirm modal.
- **SQL editor** — multi-tab, syntect syntax highlighting, JetBrains Mono
  for code, Inter for UI, autocomplete from keywords and the current
  schema. Shortcuts: `Ctrl+Enter` (run at cursor), `Ctrl+Shift+Enter`
  (run buffer), `Ctrl+/` (toggle line comment), `Ctrl+Shift+F` (format),
  `Ctrl+T` / `Ctrl+W` (tab management).
- **Results pane** — virtualized sortable grid with typed cells, NULL /
  BLOB / JSON rendering, automatic `LIMIT` pagination with "fetch next",
  CSV / TSV / SQL INSERT export to clipboard, BLOB hex viewer, and
  **edit-in-place** by detected primary key with a SQL-preview confirm.
- **UX** — friendly MySQL error messages (1062, 1146, 1213, …),
  searchable persistent history, query cancel button while running,
  light / dark / system theme, rotated logs and a panic-to-crashlog hook.

## Screenshots

*(Placeholders — drop PNGs into `docs/screenshots/` and update these
links when ready.)*

| Editor + results | Schema browser | History |
| --- | --- | --- |
| ![editor](docs/screenshots/editor.png) | ![schema](docs/screenshots/schema.png) | ![history](docs/screenshots/history.png) |

## Install

### Arch Linux (AUR)

```sh
yay -S rysql       # once the package is published
# or, from this repo:
cd packaging/aur && makepkg -si
```

### Debian / Ubuntu

Download the `.deb` from the latest
[release](https://github.com/arongarcia/rysql/releases) and:

```sh
sudo apt install ./rysql_0.6.0-1_amd64.deb
```

### AppImage (any glibc-based Linux)

```sh
chmod +x RySQL-0.6.0-x86_64.AppImage
./RySQL-0.6.0-x86_64.AppImage
```

### Windows

Download `rysql-windows-x86_64.exe` from the latest release.
A signed MSI installer is planned for a future minor release.

### macOS

Not yet packaged; build from source (below) works on Apple Silicon and
Intel Macs.

## Build from source

Requirements: Rust 1.85+ and the system libraries that egui needs.

```sh
# Debian / Ubuntu deps:
sudo apt install libxkbcommon-dev libgtk-3-dev libwayland-dev \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libgl1-mesa-dev libdbus-1-dev pkg-config

git clone https://github.com/arongarcia/rysql
cd rysql
cargo run --release -p rysql-ui
```

To build redistributable packages:

```sh
# Linux .deb
cargo install --locked cargo-deb
cargo build --release --bin rysql
cargo deb -p rysql-ui --no-build

# AppImage
bash scripts/build-appimage.sh

# Windows .exe cross-built from Linux
cargo install --locked cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc -p rysql-ui
```

## Where files live

| Purpose | Path |
| --- | --- |
| Connection profiles | `~/.config/rysql/connections.toml` |
| App settings | `~/.config/rysql/settings.toml` |
| Query history | `~/.local/share/rysql/history.json` |
| Logs | `~/.cache/rysql/logs/rysql.log.YYYY-MM-DD` |
| Crash reports | `~/.cache/rysql/crashes/panic-<ts>.txt` |
| Passwords | OS keyring under service `rysql` |

Paths follow `directories` defaults; on Windows they land under
`%APPDATA%\rysql\rysql\` and `%LOCALAPPDATA%\rysql\rysql\`.

## Architecture

Four crates in a Cargo workspace:

| Crate | Role |
| --- | --- |
| `rysql-core` | Profiles, settings, history, secret store. No I/O concurrency. |
| `rysql-db`   | sqlx-backed MySQL/MariaDB layer; one actor per connection; pinned single connection per pool so session state survives multi-statement scripts. |
| `rysql-sql`  | syntect-based highlighter, SQL keyword corpus, statement splitter, pagination helpers. |
| `rysql-ui`   | eframe/egui frontend. Splits work between a Tokio runtime (long-lived on a dedicated thread) and the egui event loop through a small bridge that streams `UiEvent`s back. |

## Contributing

Pull requests welcome. Please run `cargo fmt`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace` before
opening one.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option.
