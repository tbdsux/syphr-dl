# How the app works

Syphr is one GPUI window (`SyphrApp`) that owns the download list, plus a settings window. All downloading happens off the UI thread in the [engine](engine.md); the UI polls it twice a second.

## Source layout

| Path | What it does |
| --- | --- |
| `src/main.rs` | Entry point: app identity, fonts, themes, menus, actions, rebindable shortcuts |
| `src/download.rs` | `Download` — one row's state: `Status`, `Kind`, `Source`, progress, speed, ETA |
| `src/engine/` | Download jobs. See [engine.md](engine.md) |
| `src/tools.rs` | Finds and installs yt-dlp and ffmpeg. See [tools.md](tools.md) |
| `src/ui/app/mod.rs` | `SyphrApp`: the list, selection, filters, and the `pump` that drives jobs |
| `src/ui/app/views.rs` | Drawing: title bar, sidebar, toolbar, details card, bulk bar, status bar |
| `src/ui/app/selection.rs` | Finder-style multi-select (click, ⌘-click, Shift-click) |
| `src/ui/add_download.rs` | The link field in the title bar and its drop-down panel |
| `src/ui/downloads_table.rs` | The downloads table (columns, row rendering) |
| `src/ui/settings.rs` | `AppSettings`, the settings window, themes and appearance |

## Startup

`main` creates the GPUI app, sets the identity `dev.tbdsux.syphr-dl` / "Syphr", loads the Geist fonts and bundled themes, reads settings, binds shortcuts, builds the menus, and opens the main window. Syphr's own copy of yt-dlp is updated in the background (`yt-dlp -U`).

## Adding a download

1. The link field (⌘N) opens in the title bar. With *auto paste* on, a copied link is filled in.
2. After typing settles for 400 ms, the link is looked up:
   - **Media sites** (`ytdlp::handles`: YouTube, Vimeo, Twitch, SoundCloud, Dailymotion, X) run `yt-dlp -J`. The format list becomes a few choices: *Best quality*, one per resolution, and *Audio only*.
   - **Everything else** gets a one-byte ranged GET (`engine::inspect`) that reports the file name, size, and whether the server supports byte ranges.
3. The panel shows the result. You can rename the file, pick a quality, and change the save folder.
4. Submitting adds a `Download` with status `Queued` to the top of the list.

`kind_for` sorts each download into Video, Music, Document, Program or Archive by extension, which drives the sidebar filters and icons.

## The pump

`SyphrApp::pump` runs every 500 ms and is the only place jobs start or stop. For each download, oldest first:

1. Collect a finished job's result: mark it Completed (with its final path and size) or Failed, and notify.
2. Reconcile status with the job:
   - `Queued`/`Downloading` with no running job and a free slot → start a job (`Request` built from current settings).
   - `Downloading` with no free slot → back to `Queued`.
   - `Paused`/`Failed`/`Completed` with a running job → stop it.
3. Copy the job's progress (bytes, smoothed speed, per-part fractions) into the row.

Pausing, resuming, retrying and removing only change `Status` (or remove the row); the pump does the rest on its next tick. Removing a row drops its `Job`, which aborts it.

Settings such as the speed limit and connection count apply to downloads started after the change.

## Statuses

```
Queued ──slot free──▶ Downloading ──ok──▶ Completed
   ▲                   │    │
   └──no slot──────────┘    └──error──▶ Failed ──retry──▶ Queued
                Paused ◀──pause / resume──▶ Downloading
```

## Notifications

A finished or failed download shows an in-window toast when Syphr is focused, otherwise a system notification. Completion notices can be turned off in Settings; failures always show.

## Closing

Closing the window while anything is downloading or queued asks first.

## Settings

Stored as JSON at `~/Library/Application Support/Syphr/settings.json`. An unreadable file is ignored (and logged) and defaults are used.

| Setting | Default | Notes |
| --- | --- | --- |
| Theme | System | Bundled themes or follow the system |
| Font size | 13 | |
| Download folder | `~/Downloads` | |
| Max concurrent | 3 | Downloads running at once |
| Speed limit | 0 (unlimited) | KB/s, shared across one download's connections |
| Connections | 8 | Byte-range parts for direct links, `-N` for yt-dlp |
| yt-dlp / ffmpeg path | `yt-dlp` / `ffmpeg` | Bare names are searched; see [tools.md](tools.md) |
| Notify on complete | on | |
| Auto paste | on | |
| Shortcuts | | Per-action overrides; empty turns one off |

## Limits

- The download list lives in memory only; it's empty after a restart. Partial files stay on disk.
- Some pieces are still macOS-specific: `open -R` (Show in Finder), `ditto`, `shasum`, and the bundled ffmpeg builds. Releases are built for macOS today.
