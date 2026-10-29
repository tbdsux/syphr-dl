# Tools: yt-dlp and ffmpeg

Media downloads need [yt-dlp](https://github.com/yt-dlp/yt-dlp), and every quality except *Audio only* also needs [ffmpeg](https://ffmpeg.org) to merge video and audio. Direct downloads need neither.

## Finding them

`tools::find` resolves the yt-dlp and ffmpeg settings:

- A full path (contains `/`, or `\` on Windows) is used as-is if the file exists.
- A bare name (`yt-dlp`, the default) is searched in `PATH`, then `/opt/homebrew/bin` and `/usr/local/bin` (apps opened from Finder don't get the shell's `PATH`; skipped on Windows), then Syphr's own folder. On Windows, `yt-dlp` also matches `yt-dlp.exe`.

So a Homebrew install always wins over Syphr's copy.

## Installing them

When either is missing, Settings and the add-download panel show **Install required tools**. It downloads only what's missing into:

```
~/Library/Application Support/Syphr/bin/   (macOS)
%APPDATA%\Syphr\bin\                       (Windows)
```

and points the settings at the installed copies.

| Tool | Source | Verified against |
| --- | --- | --- |
| yt-dlp | Latest `yt-dlp_macos` / `yt-dlp.exe` from yt-dlp's GitHub releases | The release's `SHA2-256SUMS` |
| ffmpeg + ffprobe (macOS) | Pinned 9.0.2 builds from ffmpeg.martin-riedl.de, per architecture (arm64 / x86_64) | SHA-256 hashes pinned in `src/tools.rs` |
| ffmpeg + ffprobe (Windows) | Pinned 9.0.2 essentials build (x64) from gyan.dev's GitHub releases | SHA-256 hash pinned in `src/tools.rs` |

Downloads are checked in memory before anything is written, so a file that fails its checksum never reaches the disk. The ffmpeg zips are unpacked with the OS's own tools: `ditto` on macOS, `tar.exe` on Windows (only `ffmpeg.exe` and `ffprobe.exe` are extracted).

To update ffmpeg, change the URLs and their hashes in `src/tools.rs` together.

## Keeping yt-dlp current

Sites change often and break old yt-dlp versions. At launch, if the configured yt-dlp is Syphr's own copy, Syphr runs `yt-dlp -U` in the background. A Homebrew copy is left for Homebrew to update.
