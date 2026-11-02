# Tools: yt-dlp and ffmpeg

Media downloads need [yt-dlp](https://github.com/yt-dlp/yt-dlp), and every quality except *Audio only* also needs [ffmpeg](https://ffmpeg.org) to merge video and audio. Direct downloads need neither.

## Finding them

`tools::find` resolves the yt-dlp and ffmpeg settings:

- A full path (contains `/`, or `\` on Windows) is used as-is if the file exists.
- A bare name (`yt-dlp`, the default) is searched in `PATH`, then `/opt/homebrew/bin` and `/usr/local/bin` (apps opened from Finder don't get the shell's `PATH`; skipped on Windows), then Syphr's own folder. On Windows, `yt-dlp` also matches `yt-dlp.exe`.

So a Homebrew install always wins over Syphr's copy.

Each path in Settings has a **Select…** button that opens a file picker and stores the chosen file's full path.

## Installing them

When either is missing, Settings and the add-download panel show **Install required tools**. It downloads only what's missing into:

```
~/Library/Application Support/Syphr/bin/   (macOS)
%APPDATA%\Syphr\bin\                       (Windows)
~/.config/Syphr/bin/                       (Linux, or $XDG_CONFIG_HOME/Syphr/bin/)
```

and points the settings at the installed copies.

| Tool | Source | Verified against |
| --- | --- | --- |
| yt-dlp | Latest `yt-dlp_macos` / `yt-dlp.exe` / `yt-dlp_linux` (`_aarch64` on Arm) from yt-dlp's GitHub releases | The release's `SHA2-256SUMS` |
| ffmpeg + ffprobe (macOS) | Pinned 9.0.2 builds from ffmpeg.martin-riedl.de, per architecture (arm64 / x86_64) | SHA-256 hashes pinned in `src/tools.rs` |
| ffmpeg + ffprobe (Windows) | Pinned 9.0.2 essentials build (x64) from gyan.dev's GitHub releases | SHA-256 hash pinned in `src/tools.rs` |
| ffmpeg (Linux) | Not installed: the installer asks for the distro package instead | — |

Downloads are checked in memory before anything is written, so a file that fails its checksum never reaches the disk. The ffmpeg zips are unpacked with the OS's own tools: `ditto` on macOS, `tar.exe` on Windows (only `ffmpeg.exe` and `ffprobe.exe` are extracted).

To update ffmpeg, change the URLs and their hashes in `src/tools.rs` together. This includes `FFMPEG_LINUX`.

## Full builds

Each release also has a `-full` download that ships both tools inside it: `Syphr.app/Contents/Resources/tools/` on macOS, `tools/` beside the executable on Windows and Linux. On first launch, whatever isn't already found is copied into the folder above and the settings point at it, with no network needed. **Install required tools** copies from there too.

The tools run from the data folder, not from inside the app, because yt-dlp has to update itself and app updates always install the regular build. The copies are rewritten rather than file-copied, so a DMG's quarantine flag doesn't follow them and get them blocked by macOS.

The builds come from `syphr-dl --fetch-tools <dir>`, which `release.yml` runs, so they use the same URLs and checksums as the installer. On Linux that adds a pinned static ffmpeg 7.0.2 from johnvansickle.com, used only for this package. The regular Linux build still asks for the distro package.

The bundled ffmpeg must not be built with `--enable-nonfree`, because that can't be redistributed. Check a new pin's configuration before bumping it. `licenses/` (the GPL text and a notice of where each tool comes from) is copied into the `tools` folder of every full build, and

## Keeping yt-dlp current

Sites change often and break old yt-dlp versions. At launch, if the configured yt-dlp is Syphr's own copy, Syphr runs `yt-dlp -U` in the background. A Homebrew copy is left for Homebrew to update.
