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
| `src/ui/app/views.rs` | Drawing: title bar, sidebar, toolbar, bulk bar |
| `src/ui/app/details.rs` | The details window a double-clicked row opens, one per download |
| `src/ui/app/selection.rs` | Finder-style multi-select (click, ⌘/Ctrl-click, Shift-click) |
| `src/ui/add_download.rs` | The link field in the title bar and its drop-down panel |
| `src/ui/batch.rs` | The batch dialog: a checklist of links from a list, a wildcard pattern or a web page |
| `src/links.rs` | Link lists, wildcard patterns, the clipboard watcher's check, and the page grabber |
| `src/instance.rs` | Command-line arguments, `syphr://` links, and handing a second launch's links to the running app |
| `src/ui/downloads_table.rs` | The downloads table (columns, sorting, row rendering) |
| `src/ui/settings.rs` | `AppSettings`, the settings window, themes and appearance |
| `src/store.rs` | `Store`: the download list and per-download log in SQLite |
| `src/update.rs` | Checks GitHub for newer releases and installs them. See [updates.md](updates.md) |
| `src/tray.rs` | The menu bar icon (macOS) and notification-area icon (Windows, Linux) |

## Startup

`main` creates the GPUI app, sets the identity `dev.tbdsux.syphr-dl` / "Syphr", loads the DM Sans fonts and bundled themes, reads settings, binds shortcuts, builds the menus, opens the main window, and adds the tray icon. Syphr's own copy of yt-dlp is updated in the background (`yt-dlp -U`), and `update::start` begins checking for app updates.

## Running in the background

Closing the main window quits Syphr unless a download is running or queued; then the app hides on macOS and minimizes elsewhere, so downloads keep going (`close_main_window`). The tray icon (`tray::install`, built on `tray-icon`) lists ongoing downloads with their progress, running ones first and up to eight, and a click on one opens its details window. Closing a details window leaves the download running. Below them it offers *Show Syphr*, *New Download…*, *Pause All*, *Resume All*, *Open Downloads Folder*, *Settings…* and *Quit Syphr*; on Windows and Linux a left click also brings the window back. On macOS the icon is a template image cut from the app icon's glyph, so the menu bar tints it. Linux uses the StatusNotifierItem (KSNI) backend, which needs no GTK but does need a desktop that shows those icons.

## Adding a download

1. The link field (⌘N, Ctrl+N on Windows and Linux) opens in the title bar. With *auto paste* on, a copied link is filled in.
2. After typing settles for 400 ms, the panel opens and the link is looked up. A pasted link opens the panel at once, but the lookup still waits:
   - **Media sites** (`ytdlp::handles`: YouTube, Vimeo, Twitch, SoundCloud, Dailymotion, X) run `yt-dlp -J`. The format list becomes a few choices: *Best quality*, one per resolution, and *Audio only*.
   - **Everything else** gets a one-byte ranged GET (`engine::inspect`) that reports the file name, size, and whether the server supports byte ranges.
3. The panel shows the result. You can rename the file, pick a quality, and change the save folder.
4. Submitting adds a `Download` with status `Queued` to the top of the list.

`kind_for` sorts each download into Video, Music, Document, Program or Archive by extension, which drives the sidebar filters and icons.

### Other ways in

Every way below ends in one of three places: one link opens the panel above, filled in; several open the **batch dialog**, a checklist of links (all checked) with a save folder, whose *Add* puts the checked ones at the top of the list as `Queued`; and *silent* requests skip both and add straight away (`SyphrApp::take_request`). Links added without the panel aren't looked up first (`unchecked_download`): the name comes from the link until the server or yt-dlp names the file, and video sites get yt-dlp's best quality (`bv*+ba/b`).

- **Clipboard watching.** While Syphr runs it reads the clipboard every second. A new copy that is a single link to a video site (`ytdlp::handles`) or to a file whose extension is in *File types to catch* (`links::offered_link`) shows a system notification, *Download the copied link?*. Clicking it brings Syphr forward with the panel filled; its *Download* button adds the link silently. Each copy is offered once, and nothing is offered while a Syphr window is in front, since those copies are Syphr's own (*Copy links*, the link field) or the user's within it. What was on the clipboard at launch isn't offered.
- **⌘N with several copied links.** With *auto paste* on and the field empty, a clipboard holding more than one link opens the batch dialog instead.
- **Drag and drop.** Dropping files on the download list reads them: a `.webloc` or `.url` web shortcut gives its link, anything else is read as a link list. Files over 5 MB are skipped, in case a download itself was dropped. GPUI only delivers dropped files, so links dragged straight from a browser don't arrive.
- **Import and export.** File → *Import Links…* reads `.txt` files the same way. A link list has links separated by spaces or new lines; lines starting with `#` are comments, anything that isn't an `http(s)` link is ignored, and repeats are dropped (`links::parse_list`). *Export Links…* saves the selected downloads' links, or every download's when none is selected, one per line, oldest first.
- **Batch download.** File → *Batch Download…* takes one address with ranges such as `img[1-50].jpg`, `img[001-120].jpg` (the leading zeros set the padding) or `[a-z]`; several ranges multiply, a range can run backwards, and brackets that aren't ranges (an IPv6 host) stay as typed. The list updates as you type, up to 10,000 links (`links::expand_pattern`).
- **Grab links from a page.** File → *Grab Links from Page…* fetches the page through the engine's client and lists every `href` and `src` that resolves to a web link, without fragments. It can follow links to other pages on the same host one or two levels down, at most 30 pages, and keeps only the file types listed (empty keeps everything; the list starts as *File types to catch*). Only the first page failing is an error. The scan is a plain attribute search, so `<base href>`, `srcset` and links built by scripts are missed.

### Command line and other launches

```
syphr-dl [LINK...] [--dir FOLDER] [--name FILE] [--silent] [--queue-only]
```

Links are `http(s)` links or `syphr://add?url=LINK`. `--dir` saves to another folder (a relative path is resolved against the current one), `--name` names the file (one link only), `--silent` adds without the panel, and `--queue-only` adds paused, also without the panel. `--help` prints this; `--fetch-tools DIR` is the release build's step (see [tools.md](tools.md)).

Only one Syphr runs at a time. At startup the app listens on a loopback TCP port and writes the port and a random secret to `instance` in Syphr's folder, readable only by the user. A later launch that finds a running app sends it the request with the secret, and exits; with no links it just brings the window forward. A stale file (after a crash) fails to connect, and that launch starts normally. The updater restarts with `--new-instance`, which skips this so the new version doesn't hand itself to the old one.

On macOS the app registers the `syphr` URL scheme (`CFBundleURLTypes` in the release's `Info.plist`), so opening `syphr://add?url=…` anywhere brings Syphr forward with the panel filled. Only the link is taken from such a URL: any web page can open one, so it never adds silently and never picks the folder. Windows and Linux don't register the scheme yet.

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
3. Copy the job's progress (bytes, smoothed speed, per-part fractions) into the row, and add the speed to `speed_history`, which the details window draws as the last minute's graph. The history is in memory only, so it starts empty after a restart.

Pausing, resuming, retrying and removing only change `Status` (or remove the row); the pump does the rest on its next tick. Removing a row hands its `Job` to `engine::discard`, which aborts it and deletes `partial/<id>/`. "Remove and delete file…" in the row's menu also deletes the saved file, after a confirmation.

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

Closing the window quits, unless anything is downloading or queued; see the tray section above for where the app goes then.

## Saved list and log

The list and a log per download live in SQLite at `downloads.db` in Syphr's folder (see Settings below), through `rusqlite` with SQLite built in. `SyphrApp::new` loads it, newest first, and continues ids from the highest one. Every `sync_table` calls `Store::sync`, which writes only rows whose saved fields changed (speed and part fractions aren't saved, so a running download rewrites just its byte count) and deletes removed rows with their log, in one transaction.

The `log` table gets a line when a download is added, on every status change, with the saved path on completion, and with the error on failure. The engine adds its own lines through `Job::take_log`, which the pump drains each tick: yt-dlp's `[extractor] ...` output and stderr, and for direct links the probe request and response, each part's ranged request and status, where the file goes, and retries. The details window shows it, newest first, open by default.

Downloads that were running or queued when Syphr quit start again on launch. If the database can't be opened, Syphr logs why and keeps the list in memory for that session.

## Settings

Stored as JSON at `settings.json` in Syphr's folder: `~/Library/Application Support/Syphr` on macOS, `%APPDATA%\Syphr` on Windows, `$XDG_CONFIG_HOME/Syphr` (or `~/.config/Syphr`) on Linux. An unreadable file is ignored (and logged) and defaults are used.

| Setting | Default | Notes |
| --- | --- | --- |
| Theme | System | Bundled themes or follow the system |
| Font size | 13 (15 on Linux) | 11–20 pt; the whole UI scales in rems. View menu and ⌘= / ⌘- / ⌘0 step it |
| Download folder | `~/Downloads` | |
| Max concurrent | 3 | Downloads running at once |
| Speed limit | 0 (unlimited) | KB/s, shared across one download's connections |
| Connections | 8 | Byte-range parts for direct links, `-N` for yt-dlp |
| Proxy | None | Type, host, port, username, password for all traffic; the password is stored as plain text. See [engine.md](engine.md#proxy) |
| yt-dlp / ffmpeg path | `yt-dlp` / `ffmpeg` | Bare names are searched; see [tools.md](tools.md) |
| Preferred video codec | No preference | H.264, VP9 or AV1 is picked first at each resolution |
| Video file type | Automatic | MP4 or MKV repackages video downloads with ffmpeg |
| Extra yt-dlp options | | Split like a shell line, added before Syphr's own arguments |
| Notify on complete | on | |
| Auto paste | on | ⌘N fills the field with a copied link, or opens the batch dialog for several |
| Watch the clipboard | on | Offer copied file and video links; see [Other ways in](#other-ways-in) |
| File types to catch | archives, programs, media, PDF | Extensions the clipboard watcher offers and the grabber starts with |
| Shortcuts | | Per-action overrides; empty turns one off |

## Limits

- The tool installer only has macOS and Windows builds; on Linux, install yt-dlp and ffmpeg yourself.
- On Linux, *Show in folder* opens the folder without selecting the file.
- System notifications need an app bundle on macOS, so under `cargo run` clipboard offers (like download notices) don't show.
- Links dragged straight from a browser can't be dropped; drop a web shortcut or a link list instead.
- FTP links aren't caught or accepted: the engine only downloads over HTTP(S).
