<img src="assets/icon/icon.png" width="96" alt="Syphr icon">

# Syphr

A desktop download manager app. Paste a link and Syphr downloads it: plain files over parallel HTTP connections, and video or audio from sites like YouTube through [yt-dlp](https://github.com/yt-dlp/yt-dlp).

Built in Rust with [GPUI Kit](https://gpui-kit.com).

## Features

- **Fast direct downloads.** Files are split into byte ranges and fetched over up to 8 connections at once, like IDM.
- **Pause and resume.** A paused download keeps its finished parts and continues where it stopped.
- **Video and audio.** YouTube, Vimeo, Twitch, SoundCloud, Dailymotion and X links show a list of qualities (1080p60, 720p, audio only, …) with estimated sizes.
- **Queue.** Downloads beyond the concurrent limit wait their turn, oldest first.
- **Speed limit**, per-download connection count, and a chosen save folder.
- **Tools installed for you.** If yt-dlp or ffmpeg is missing, Syphr downloads checksum-verified copies into its own folder. Your own installs (Homebrew or PATH) are used first.
- Themes (Catppuccin, Everforest, Flexoki, Gruvbox, Solarized, Tokyo Night), rebindable keyboard shortcuts, and notifications when a download finishes.

## Install

| OS | Status |
| --- | --- |
| macOS 15+ | Prebuilt DMG (Apple Silicon and Intel) |
| Linux | Build from source |
| Windows 10+ | Prebuilt zip (x64) |

### macOS

Download `Syphr-vX.Y.Z.dmg` from [Releases](../../releases), open it, and drag Syphr into Applications.

Releases aren't notarized yet, so macOS blocks the first launch. Right-click Syphr in Applications and choose **Open**, or run:

```bash
xattr -dr com.apple.quarantine /Applications/Syphr.app
```

Syphr offers to install yt-dlp and ffmpeg for you the first time you add a video link.

### Linux

There are no prebuilt Linux packages yet. Build from source (see [Linux build requirements](#linux-1)), then run `target/release/syphr-dl`.

Install yt-dlp and ffmpeg with your package manager. Syphr's built-in tool installer only downloads macOS and Windows builds.

```bash
sudo apt install yt-dlp ffmpeg
```

Distro packages of yt-dlp often lag behind, and sites break old versions. If video links fail, install the latest release from [yt-dlp's releases](https://github.com/yt-dlp/yt-dlp/releases) instead.

### Windows

Download `Syphr-vX.Y.Z-windows-x64.zip` from [Releases](../../releases), unzip it, and run `syphr-dl.exe`.

Releases aren't code-signed yet, so SmartScreen may warn on first launch. Choose **More info**, then **Run anyway**.

Syphr offers to install yt-dlp and ffmpeg for you the first time you add a video link.

## Build from source

Requires [Rust](https://rustup.rs) 1.92 or later. Then install your platform's build requirements below and run:

```bash
cargo build --release
```

### macOS

Xcode Command Line Tools:

```bash
xcode-select --install
```

### Windows

[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the **Desktop development with C++** workload.

### Linux

A graphical Wayland or X11 session and a working Vulkan driver. On Ubuntu 24.04:

```bash
sudo apt install -y gcc g++ clang pkg-config libfontconfig-dev libwayland-dev libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev vulkan-validationlayers libvulkan1
```

Other distributions need the equivalent development packages.

### Development

| Task | Command |
| --- | --- |
| Run | `cargo run` |
| Test | `cargo test` |
| Network tests | `cargo test -- --ignored` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Format | `cargo fmt` |

## Documentation

- [How the app works](docs/architecture.md)
- [The download engine](docs/engine.md)
- [Tools: yt-dlp and ffmpeg](docs/tools.md)
- [CI and releases](docs/releasing.md)

## License

[MIT](LICENSE). Bundled Geist fonts are under the [SIL Open Font License](assets/fonts/OFL.txt).
