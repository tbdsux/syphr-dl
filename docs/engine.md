# The download engine

`src/engine/` turns a `Request` into a file on disk. It has two backends:

- **Direct** (`direct.rs`) — plain HTTP(S) files, fetched in parallel byte ranges.
- **yt-dlp** (`ytdlp.rs`) — media pages, run as a child process.

A request with a `selector` (a yt-dlp `-f` format) goes to yt-dlp; without one it's a direct download.

## Jobs

```rust
pub struct Request {
    url, folder,
    selector: Option<String>,   // yt-dlp -f; None = direct
    file_name: Option<String>,  // typed name; None = server's name / video title
    ytdlp_bin, ffmpeg_bin,
    limit_bps: u64,             // 0 = unlimited
    fragments: u64,             // parallel connections
}
```

A `Job` runs one request on a shared 4-thread Tokio runtime (`RUNTIME`) with one shared `reqwest` client (`CLIENT`: 15 s connect timeout, 30 s read timeout, `Syphr/<version>` user agent).

The UI never awaits a job. It polls:

| Method | Returns |
| --- | --- |
| `start(request)` | Spawns the run (stopping any previous one) |
| `stop()` | Aborts the task; finished parts are kept |
| `is_running()` | Whether the task is alive |
| `take_result()` | `Some(Ok(path) / Err(message))` once, when the run ends on its own |
| `received()` / `total()` | Bytes so far / total if known |
| `fragments()` | Each part's finished fraction, for the progress bar |
| `speed()` | Smoothed bytes per second (exponential average: 60 % previous, 40 % latest) |

State shared between the task and the UI (`Shared`) is atomics and mutexes: total, received, the server's file name, the part layout, and the result. Because the layout lives in `Shared`, not the task, it survives `stop()`, which is what makes resume work.

## Direct downloads

### 1. Inspect

`inspect(url)` sends `GET` with `Range: bytes=0-0`:

- `206 Partial Content` → ranges are supported; the size comes from `Content-Range` (`bytes 0-0/12345`).
- `200 OK` → no ranges; the size is `Content-Length`, if any.
- Ranges are only used when the size is known.

The same response gives the file name (see [Naming](#naming)) and content type. The add-download panel calls this too, to show the name and size before you add the link.

### 2. Plan the parts

On the first run, `probe` picks a free path in the folder and splits the file:

| Server | Parts |
| --- | --- |
| Ranges + known size | `split(total, fragments)`: up to `fragments` parts, each at least 1 MiB |
| Known size, no ranges | One part, `0..total` |
| Unknown size | One open-ended part, read until the server closes |

`split` makes equal parts (the last may be shorter) that cover every byte exactly once; a unit test checks this.

### 3. Fetch

All bytes go into `<name>.part` in Syphr's data folder, under `partial/<hash of destination folder>/`. Each unfinished part runs as its own task:

- Ranged: `Range: bytes=<start + done>-<end>`, so a part resumes from its own offset. A server that answers anything other than 206 fails the part.
- Not ranged: the file is truncated and the single part restarts from zero.
- Chunks are written with `pwrite` (`write_all_at`) at the part's offset, so parts never need locking. Each part's `done` counter is updated after every write.
- **Speed limit:** `limit_bps` is split evenly across the running parts. Each part sleeps whenever it is ahead of `bytes / limit` seconds.
- **Retries:** a failed part retries up to 5 times, waiting 2, 4, 8, 16, 16 s. Retries resume from the part's offset.
- A part with a known end that stops short is an error, not a success.

If any part fails all its retries, the job fails with that part's error.

### 4. Finish

When every part is done the file is synced, then `.part` is moved to the final name (renamed, or copied and deleted when the destination is on another volume). If something else took that name meanwhile, the next free name is used.

### Pause and resume

`stop()` aborts the part tasks mid-chunk. Since `done` is only advanced after a write lands, the counters match what's on disk. The next `start()` reuses the same layout, reopens `.part` without truncating, and starts only the unfinished parts. If the `.part` file was deleted while paused, every counter resets to zero.

## yt-dlp downloads

### Choosing formats

`probe(bin, url)` runs `yt-dlp -J --no-playlist` and `parse` reduces the format list to a few choices:

- **One per resolution**, highest first: the best video stream at that height (higher fps, then bigger size).
- Video without audio is paired with the best audio in a compatible container (mp4+m4a, webm+webm), otherwise the best audio overall, merged into mkv.
- Selectors use exact format ids (`137+140/b[height=1080]`), so the file matches the label; the `/b[height=…]` fallback covers formats that vanish between lookup and download.
- **Best quality** copies the top resolution. **Audio only** is the highest-bitrate audio stream.
- Each choice shows its container and estimated size (`MP4 · ~48.2 MB`).

### Running

`run_ytdlp`:

1. If the selector merges two streams (`+`), checks that ffmpeg runs, and fails with a pointer to *Install required tools* if not.
2. Runs yt-dlp with:
   - `-f <selector> -N <fragments> --no-playlist`
   - `-r <limit>` when a speed limit is set
   - `--ffmpeg-location` when ffmpeg is a full path
   - a progress template that prints `downloaded total` per line, and `--print after_move:filepath` for the final path
   - output into the staging folder `<data folder>/partial/<hash of destination folder>/` as `<typed name or title>.<ext>`
3. Reads progress from stdout. Video and audio download one after the other, each counting from zero, so a drop in the counter starts a new base. The last stderr line is kept as the error message.
4. On success, moves the file into the destination folder under a free name and removes the staging folder if it's empty.

yt-dlp is killed when the job is stopped (`kill_on_drop`). The staging folder name is stable, so yt-dlp's own `.part` files let a paused download resume.

The staging folder exists because yt-dlp skips a file that already exists instead of renaming it.

## Naming

`names.rs` decides every file name:

1. **Server name:** `Content-Disposition` (`filename*=UTF-8''…` wins over `filename=`), else the last URL path segment after redirects (percent-decoded), else `download`. With no extension, one is guessed from `Content-Type`.
2. **Typed name** wins. If it has no extension, the server's is added (`notes` + `report.pdf` → `notes.pdf`).
3. **Sanitized:** `/ \ : NUL` and control characters (plus `< > " | ? *` on Windows) become `_`, leading dots are removed, so a name can't escape the folder or hide the file.
4. **Unique:** `name.zip`, then `name (1).zip`, `name (2).zip`, … skipping names where the file *or* its `.part` exists.

## Errors

Every error is a `String` meant for the user and shown in the failure notification: HTTP errors, "The server stopped accepting partial requests.", "The connection closed before the part finished.", yt-dlp's last error line, or the missing-ffmpeg message.

## Tests

`cargo test` runs the offline tests: part splitting, naming, format parsing, progress parsing, and the ffmpeg check.

`cargo test -- --ignored` also runs live tests against the network: an 8-part download, stop-and-resume, `Content-Disposition` naming, and yt-dlp downloads (needs yt-dlp and ffmpeg installed).
