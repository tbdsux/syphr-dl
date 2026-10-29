# Tools: yt-dlp and ffmpeg

Media downloads need [yt-dlp](https://github.com/yt-dlp/yt-dlp), and every quality except *Audio only* also needs [ffmpeg](https://ffmpeg.org) to merge video and audio. Direct downloads need neither.

## Finding them

`tools::find` resolves the yt-dlp and ffmpeg settings:

- A full path (contains `/`) is used as-is if the file exists.
- A bare name (`yt-dlp`, the default) is searched in `PATH`, then `/opt/homebrew/bin` and `/usr/local/bin` (apps opened from Finder don't get the shell's `PATH`), then Syphr's own folder.

So a Homebrew install always wins over Syphr's copy.

## Installing them

When either is missing, Settings and the add-download panel show **Install required tools**. It downloads only what's missing into:

```
~/Library/Application Support/Syphr/bin/
```

and points the settings at the installed copies.

| Tool | Source | Verified against |
| --- | --- | --- |
| yt-dlp | Latest `yt-dlp_macos` from yt-dlp's GitHub releases | The release's `SHA2-256SUMS` |
| ffmpeg + ffprobe | Pinned 9.0.2 builds from ffmpeg.martin-riedl.de, per architecture (arm64 / x86_64) | SHA-256 hashes pinned in `src/tools.rs` |

A file that fails its checksum is deleted before it can run. The ffmpeg zips are unpacked with macOS's built-in `ditto`.

To update ffmpeg, change the URLs and their hashes in `src/tools.rs` together.

## Keeping yt-dlp current

Sites change often and break old yt-dlp versions. At launch, if the configured yt-dlp is Syphr's own copy, Syphr runs `yt-dlp -U` in the background. A Homebrew copy is left for Homebrew to update.
