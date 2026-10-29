# Syphr DL

Download manager app in Rust (edition 2024), built on [GPUI Kit](https://crates.io/crates/gpui-kit). Downloads direct links over HTTP and media pages through yt-dlp (+ ffmpeg), which Syphr installs into Application Support when missing.

## Layout

- `src/main.rs` — app entry, actions, keybindings, asset source
- `src/engine/` — link inspection and download jobs (`direct.rs` for HTTP, `ytdlp.rs` for media, `names.rs` for file naming)
- `src/download.rs` — download state (status, kind, source)
- `src/tools.rs` — finds/installs yt-dlp and ffmpeg (pinned SHA-256 checksums)
- `src/ui/` — windows, downloads table, add-download dialog, settings
- `docs/` — how the app and engine work; keep it in sync when behavior changes
- `assets/` — bundled fonts (Geist) and themes

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Lint: `cargo clippy`
- Format: `cargo fmt` (`rustfmt.toml`: max width 120)

## Skills

Project skills live in `.agents/skills/`. Read the relevant one before working in that area:

- `gpui-kit` — GPUI Kit components, state, actions, and the normative Coding Guides
- `gpui-kit-design-guides` — Design Guides; read before any UI/layout/visual change
- `rust-patterns` — idiomatic Rust: ownership, errors, traits, concurrency

## Conventions

- Match surrounding code: terse, comments only for the non-obvious why.
- Don't change pinned tool URLs without updating their checksums together.
