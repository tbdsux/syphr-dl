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
| `src/ui/app/selection.rs` | Finder-style multi-select (click, ⌘/Ctrl-click, Shift-click) |
| `src/ui/add_download.rs` | The link field in the title bar and its drop-down panel |
| `src/ui/downloads_table.rs` | The downloads table (columns, sorting, row rendering) |
| `src/ui/settings.rs` | `AppSettings`, the settings window, themes and appearance |
| `src/store.rs` | `Store`: the download list and per-download log in SQLite |
| `src/update.rs` | Checks GitHub for newer releases and installs them. See [updates.md](updates.md) |
| `src/tray.rs` | The menu bar icon (macOS) and notification-area icon (Windows, Linux) |

## Startup

`main` creates the GPUI app, sets the identity `dev.tbdsux.syphr-dl` / "Syphr", loads the Geist fonts and bundled themes, reads settings, binds shortcuts, builds the menus, adds the tray icon, and opens the main window. Syphr's own copy of yt-dlp is updated in the background (`yt-dlp -U`), and `update::start` begins checking for app updates.

## Running in the background

Closing the main window on macOS hides the app, so downloads keep going; elsewhere it minimizes while downloads are running and quits otherwise (`allow_close`). The tray icon (`tray::install`, built on `tray-icon`) offers *Show Syphr* and *Quit Syphr*; on Windows and Linux a left click also brings the window back. On macOS the icon is a template image cut from the app icon's glyph, so the menu bar tints it. Linux uses the StatusNotifierItem (KSNI) backend, which needs no GTK but does need a desktop that shows those icons.

## Adding a download

1. The link field (⌘N, Ctrl+N on Windows and Linux) opens in the title bar. With *auto paste* on, a copied link is filled in.
2. After typing settles for 400 ms, the link is looked up:
   - **Media sites** (`ytdlp::handles`: YouTube, Vimeo, Twitch, SoundCloud, Dailymotion, X) run `yt-dlp -J`. The format list becomes a few choices: *Best quality*, one per resolution, and *Audio only*.
   - **Everything else** gets a one-byte ranged GET (`engine::inspect`) that reports the file name, size, and whether the server supports byte ranges.
3. The panel shows the result. You can rename the file, pick a quality, and change the save folder.
4. Submitting adds a `Download` with status `Queued` to the top of the list.

`kind_for` sorts each download into Video, Music, Document, Program or Archive by extension, which drives the sidebar filters and icons.

## The downloads table

Rows show newest-added first. Clicking a column header (or its sort icon) cycles descending → ascending → back to newest-added; ties always keep newest-added first. The sort lives in `DownloadsTable::sort` for the session and is reapplied on every `sync_table`, so live columns like Speed reorder as they change.

Settings → Appearance → *Download list columns* hides Progress, Size, Speed, Time left or Status (`AppSettings::hidden_columns`, by column id). Columns can be dragged between the checkboxes and the actions, which stay pinned at the edges; the order is kept for the session, hidden columns included.

Until a column is resized, Name fills the table. After that, the Actions column takes the slack (its buttons stay right-aligned), so dragging an edge moves only the columns to its right, and a column can only grow into that slack: narrow Name first to make room. Name gives width back when the window gets too narrow. `DownloadsTable::layout` computes the widths. During a drag, `follow_drag` resizes Actions through the kit's `fit_col`, never `refresh`, which would reset the kit's column bounds and break the drag; columns are rebuilt only when they change (`stale`). Resized widths are kept in rems for the session.

## The pump

`SyphrApp::pump` runs every 500 ms and is the only place jobs start or stop. For each download, oldest first:

1. Collect a finished job's result: mark it Completed (with its final path and size) or Failed, and notify.
2. Reconcile status with the job:
   - `Queued`/`Downloading` with no running job and a free slot → start a job (`Request` built from current settings).
   - `Downloading` with no free slot → back to `Queued`.
   - `Paused`/`Failed`/`Completed`/`Missing` with a running job → stop it.
   - `Completed` whose file is gone from its saved path → `Missing`, and back once it reappears.
3. Copy the job's progress (bytes, smoothed speed, per-part fractions) into the row.

Pausing, resuming, retrying and removing only change `Status` (or remove the row); the pump does the rest on its next tick. Removing a row drops its `Job`, which aborts it.

Settings such as the speed limit and connection count apply to downloads started after the change.

## Statuses

```
Queued ──slot free──▶ Downloading ──ok──▶ Completed ◀──file back / file gone──▶ Missing ──download again──▶ Queued
   ▲                   │    │
   └──no slot──────────┘    └──error──▶ Failed ──retry──▶ Queued
                Paused ◀──pause / resume──▶ Downloading
```

## Notifications

A finished or failed download shows an in-window toast when Syphr is focused, otherwise a system notification. Completion notices can be turned off in Settings; failures always show.

## Closing

Closing the window while anything is downloading or queued asks first.

## Saved list and log

The list and a log per download live in SQLite at `downloads.db` in Syphr's folder (see Settings below), through `rusqlite` with SQLite built in. `SyphrApp::new` loads it, newest first, and continues ids from the highest one. Every `sync_table` calls `Store::sync`, which writes only rows whose saved fields changed (speed and part fractions aren't saved, so a running download rewrites just its byte count) and deletes removed rows with their log, in one transaction.

The `log` table gets a line when a download is added, on every status change, with the saved path on completion, and with the error on failure. Nothing in the UI shows it yet; `Store::log_for` reads it.

Downloads that were running or queued when Syphr quit start again on launch. If the database can't be opened, Syphr logs why and keeps the list in memory for that session.

## Settings

Stored as JSON at `settings.json` in Syphr's folder: `~/Library/Application Support/Syphr` on macOS, `%APPDATA%\Syphr` on Windows, `$XDG_CONFIG_HOME/Syphr` (or `~/.config/Syphr`) on Linux. An unreadable file is ignored (and logged) and defaults are used.

| Setting | Default | Notes |
| --- | --- | --- |
| Theme | System | Bundled themes or follow the system |
| Font size | 13 | 11–20 pt; the whole UI scales in rems. View menu and ⌘= / ⌘- / ⌘0 step it |
| Download folder | `~/Downloads` | |
| Max concurrent | 3 | Downloads running at once |
| Speed limit | 0 (unlimited) | KB/s, shared across one download's connections |
| Connections | 8 | Byte-range parts for direct links, `-N` for yt-dlp |
| yt-dlp / ffmpeg path | `yt-dlp` / `ffmpeg` | Bare names are searched; see [tools.md](tools.md) |
| Notify on complete | on | |
| Auto paste | on | |
| Shortcuts | | Per-action overrides; empty turns one off |

## Limits

- The tool installer only has macOS and Windows builds; on Linux, install yt-dlp and ffmpeg yourself.
- On Linux, *Show in folder* opens the folder without selecting the file.
