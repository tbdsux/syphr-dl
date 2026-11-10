# How the app works

Syphr is one GPUI window (`SyphrApp`) that owns the download list, plus a settings window. All downloading happens off the UI thread in the [engine](engine.md); the UI polls it twice a second.

## Source layout

| Path | What it does |
| --- | --- |
| `src/main.rs` | Entry point: app identity, fonts, themes, menus, actions, rebindable shortcuts |
| `src/download.rs` | `Download` — one row's state: `Status`, `Kind`, `Source`, queue and position, progress, speed, ETA |
| `src/queue.rs` | `Queue`: schedule windows (`inside`, `next_start`), download order (`reorder`), finish actions |
| `src/engine/` | Download jobs. See [engine.md](engine.md) |
| `src/tools.rs` | Finds and installs yt-dlp and ffmpeg. See [tools.md](tools.md) |
| `src/ui/app/mod.rs` | `SyphrApp`: the list, selection, filters, and the `pump` that drives jobs |
| `src/ui/app/views.rs` | Drawing: title bar, sidebar, toolbar, bulk bar |
| `src/ui/app/details.rs` | The details window a double-clicked row opens, one per download |
| `src/ui/app/queues.rs` | Queue commands, the scheduler tick, the finish countdown, and the Queues window |
| `src/ui/app/selection.rs` | Finder-style multi-select (click, ⌘/Ctrl-click, Shift-click) |
| `src/ui/add_download.rs` | The link field in the title bar and its drop-down panel |
| `src/ui/downloads_table.rs` | The downloads table (columns, sorting, row rendering) |
| `src/ui/settings.rs` | `AppSettings`, the settings window, themes and appearance |
| `src/store.rs` | `Store`: the download list and per-download log in SQLite |
| `src/update.rs` | Checks GitHub for newer releases and installs them. See [updates.md](updates.md) |
| `src/tray.rs` | The menu bar icon (macOS) and notification-area icon (Windows, Linux) |

## Startup

`main` creates the GPUI app, sets the identity `dev.tbdsux.syphr-dl` / "Syphr", loads the DM Sans fonts and bundled themes, reads settings, binds shortcuts, builds the menus, opens the main window, and adds the tray icon. Syphr's own copy of yt-dlp is updated in the background (`yt-dlp -U`), and `update::start` begins checking for app updates.

## Running in the background

Closing the main window quits Syphr unless a download is running or queued, or waits in a queue with a schedule; then the app hides on macOS and minimizes elsewhere, so downloads keep going (`close_main_window`). The tray icon (`tray::install`, built on `tray-icon`) lists ongoing downloads with their progress, running ones first and up to eight, and a click on one opens its details window. Closing a details window leaves the download running. Below them it offers *Show Syphr*, *New Download…*, *Pause All*, *Resume All*, *Open Downloads Folder*, *Settings…* and *Quit Syphr*; on Windows and Linux a left click also brings the window back. On macOS the icon is a template image cut from the app icon's glyph, so the menu bar tints it. Linux uses the StatusNotifierItem (KSNI) backend, which needs no GTK but does need a desktop that shows those icons.

## Adding a download

1. The link field (⌘N, Ctrl+N on Windows and Linux) opens in the title bar. With *auto paste* on, a copied link is filled in.
2. After typing settles for 400 ms, the panel opens and the link is looked up. A pasted link opens the panel at once, but the lookup still waits:
   - **Media sites** (`ytdlp::handles`: YouTube, Vimeo, Twitch, SoundCloud, Dailymotion, X) run `yt-dlp -J`. The format list becomes a few choices: *Best quality*, one per resolution, and *Audio only*.
   - **Everything else** gets a one-byte ranged GET (`engine::inspect`) that reports the file name, size, and whether the server supports byte ranges.
3. The panel shows the result. You can rename the file, pick a quality, and change the save folder.
4. *Start download* adds a `Download` with status `Queued` to the top of the list; *Download later* adds it as `Later`, so it waits for its queue to start. Either way it joins the queue shown in the sidebar, or Main, last in that queue's order.

`kind_for` sorts each download into Video, Music, Document, Program or Archive by extension, which drives the sidebar filters and icons.

## The downloads table

Rows show newest-added first. Clicking a column header (or its sort icon) cycles descending → ascending → back to newest-added; ties always keep newest-added first. The sort lives in `DownloadsTable::sort` for the session and is reapplied on every `sync_table`, so live columns like Speed reorder as they change.

Settings → Appearance → *Download list columns* hides Progress, Size, Speed, Time left or Status (`AppSettings::hidden_columns`, by column id). Columns can be dragged between the checkboxes and the actions, which stay pinned at the edges; the order is kept for the session, hidden columns included.

Until a column is resized, Name fills the table. After that, the Actions column takes the slack (its buttons stay right-aligned), so dragging an edge moves only the columns to its right, and a column can only grow into that slack: narrow Name first to make room. Name gives width back when the window gets too narrow. `DownloadsTable::layout` computes the widths. During a drag, `follow_drag` resizes Actions through the kit's `fit_col`, never `refresh`, which would reset the kit's column bounds and break the drag; columns are rebuilt only when they change (`stale`). Resized widths are kept in rems for the session.

## Queues and the scheduler

Every download belongs to one queue (`Download::queue`). *Main* (id 1) always exists and can't be deleted; the sidebar's *Queues* section lists it with the queues the user adds (+), and selecting one shows only its downloads, in the order they start (`Download::position`, lower first). There the toolbar offers *Start queue* (`Later` and `Paused` → `Queued`), *Stop queue* (`Downloading` and `Queued` → `Later`) and *Schedule…*, and a row's menu offers *Move up / down / to top / to bottom*. *Move to queue* is in every row's menu and puts the download last in the other queue.

*Schedule…* opens the Queues window, one page per queue: name, start and stop times, the days they apply to (or one date), how many of its downloads run at once (0 follows *Simultaneous downloads*, which still caps all queues together), and what to do when it finishes. Deleting a queue moves its downloads to Main.

`Queue::inside` says whether a time falls in the queue's window: from the start to the stop time on a chosen day (a stop earlier than the start ends the next day), from midnight when there's no start time, and just the start minute when there's no stop time. Each tick `run_schedules` compares that with the last tick: the window opening (or already open when Syphr launches) starts the queue, and it closing stops it. Editing a queue records its current state without acting, so a change never starts or stops a queue by itself.

When a download ends and its queue has nothing left that is `Downloading`, `Queued` or `Later`, the queue's finish action runs: *Quit Syphr*, *Sleep* or *Shut down*. Syphr comes to the front with a 30-second countdown dialog; *Cancel* or Escape stops it. Sleep and shut down call `pmset sleepnow` / `osascript … shut down` on macOS, `rundll32 powrprof.dll,SetSuspendState` / `shutdown /s` on Windows and `systemctl suspend` / `poweroff` on Linux.

## The pump

`SyphrApp::pump` runs every 500 ms and is the only place jobs start or stop. It first runs the queue schedules, then goes through the downloads in queue order (`position`, then oldest first):

1. Collect a finished job's result: mark it Completed (with its final path and size) or Failed, and notify.
2. Reconcile status with the job:
   - `Queued`/`Downloading` with no running job and a free slot → start a job (`Request` built from current settings). A slot is free when fewer than *Simultaneous downloads* run in all and fewer than the queue's own limit run in its queue.
   - `Downloading` with no free slot → back to `Queued`.
   - `Paused`/`Failed`/`Completed`/`Missing`/`Later` with a running job → stop it. `Later` never starts on its own.
   - `Completed` whose file is gone from its saved path → `Missing`, and back once it reappears.
3. Copy the job's progress (bytes, smoothed speed, per-part fractions) into the row, and add the speed to `speed_history`, which the details window draws as the last minute's graph. The history is in memory only, so it starts empty after a restart.
4. After the loop, run the finish action of any queue that just finished (see above).

Pausing, resuming, retrying and removing only change `Status` (or remove the row); the pump does the rest on its next tick. Removing a row hands its `Job` to `engine::discard`, which aborts it and deletes `partial/<id>/`. "Remove and delete file…" in the row's menu also deletes the saved file, after a confirmation.

Settings such as the speed limit and connection count apply to downloads started after the change.

## Statuses

```
Queued ──slot free──▶ Downloading ──ok──▶ Completed ◀──file back / file gone──▶ Missing ──download again──▶ Queued
   ▲                   │    │
   └──no slot──────────┘    └──error──▶ Failed ──retry──▶ Queued
                Paused ◀──pause / resume──▶ Downloading
Later ──start queue / schedule start──▶ Queued        Later ──start now / resume──▶ Downloading
Downloading, Queued ──stop queue / schedule stop──▶ Later        Paused ──start queue──▶ Queued
```

*Resume all* leaves `Later` downloads alone; they wait for their queue.

## Notifications

A finished or failed download shows an in-window toast when Syphr is focused, otherwise a system notification. Completion notices can be turned off in Settings; failures always show.

## Closing

Closing the window quits, unless anything is downloading or queued, or a `Later` download waits in a queue with a schedule; see the tray section above for where the app goes then.

## Saved list and log

The list and a log per download live in SQLite at `downloads.db` in Syphr's folder (see Settings below), through `rusqlite` with SQLite built in. `SyphrApp::new` loads it, newest first, and continues ids from the highest one. Every `sync_table` calls `Store::sync`, which writes only rows whose saved fields changed (speed and part fractions aren't saved, so a running download rewrites just its byte count) and deletes removed rows with their log, in one transaction.

The `log` table gets a line when a download is added, on every status change, with the saved path on completion, and with the error on failure. The engine adds its own lines through `Job::take_log`, which the pump drains each tick: yt-dlp's `[extractor] ...` output and stderr, and for direct links the probe request and response, each part's ranged request and status, where the file goes, and retries. The details window shows it, newest first, open by default.

The `queues` table holds each queue's name, schedule (minutes after midnight, a day mask, an optional date), limit and finish action; `Store::save_queues` rewrites it whenever a queue changes. `downloads` gained `queue` and `position` columns: a database from before queues gets them on open, with every download in Main ordered by id. The check looks for the column rather than a schema version.

Downloads that were running or queued when Syphr quit start again on launch; `Later` ones keep waiting. If the database can't be opened, Syphr logs why and keeps the list in memory for that session.

## Settings

Stored as JSON at `settings.json` in Syphr's folder: `~/Library/Application Support/Syphr` on macOS, `%APPDATA%\Syphr` on Windows, `$XDG_CONFIG_HOME/Syphr` (or `~/.config/Syphr`) on Linux. An unreadable file is ignored (and logged) and defaults are used.

| Setting | Default | Notes |
| --- | --- | --- |
| Theme | System | Bundled themes or follow the system |
| Font size | 13 (15 on Linux) | 11–20 pt; the whole UI scales in rems. View menu and ⌘= / ⌘- / ⌘0 step it |
| Download folder | `~/Downloads` | |
| Max concurrent | 3 | Downloads running at once, across all queues. Each queue can set a lower limit of its own in the Queues window |
| Speed limit | 0 (unlimited) | KB/s, shared across one download's connections |
| Connections | 8 | Byte-range parts for direct links, `-N` for yt-dlp |
| Proxy | None | Type, host, port, username, password for all traffic; the password is stored as plain text. See [engine.md](engine.md#proxy) |
| yt-dlp / ffmpeg path | `yt-dlp` / `ffmpeg` | Bare names are searched; see [tools.md](tools.md) |
| Preferred video codec | No preference | H.264, VP9 or AV1 is picked first at each resolution |
| Video file type | Automatic | MP4 or MKV repackages video downloads with ffmpeg |
| Extra yt-dlp options | | Split like a shell line, added before Syphr's own arguments |
| Notify on complete | on | |
| Auto paste | on | |
| Shortcuts | | Per-action overrides; empty turns one off |

## Limits

- The tool installer only has macOS and Windows builds; on Linux, install yt-dlp and ffmpeg yourself.
- On Linux, *Show in folder* opens the folder without selecting the file.
