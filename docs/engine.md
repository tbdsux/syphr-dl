# The download engine

`src/engine/` turns a `Request` into a file on disk. It has two backends:

- **Direct** (`direct.rs`) — plain HTTP(S) files, fetched in parallel byte ranges, and `ftp://` files (`ftp.rs`), fetched the same way with `REST`.
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
    access: Access,             // login, referer, cookie, user agent
}
```

A `Job` runs one request on a shared 4-thread Tokio runtime (`RUNTIME`) with one shared `reqwest` client (`client()`: 15 s connect timeout, 30 s read timeout, `Syphr/<version>` user agent).

The UI never awaits a job. It polls:

| Method | Returns |
| --- | --- |
| `start(request)` | Spawns the run (stopping any previous one) |
| `stop()` | Aborts the task; finished parts are kept |
| `set_limit(bps)` | This download's speed limit; applies at once to direct downloads |
| `is_running()` | Whether the task is alive |
| `take_result()` | `Some(Ok(path) / Err(message))` once, when the run ends on its own |
| `received()` / `total()` | Bytes so far / total if known |
| `fragments()` | Each part in file order as (share of the file, finished fraction), for the progress strips |
| `speed()` | Smoothed bytes per second (exponential average: 60 % previous, 40 % latest) |

State shared between the task and the UI (`Shared`) is atomics and mutexes: total, received, the server's file name, the part layout, and the result. Because the layout lives in `Shared`, not the task, it survives `stop()`, which is what makes resume work.

## Proxy

`AppSettings::proxy_url` turns Settings → *Proxy* (type, host, port, username, password) into a link, percent-encoding the login. `set_proxy` rebuilds the client from it when a field changes and at launch. yt-dlp gets the same link through `--proxy` for lookups, downloads and `-U`. Tool installs and update checks use the client, so they follow it too.

HTTPS means TLS to the proxy itself, which many providers, Webshare included, don't offer; an HTTP proxy still carries `https://` links, encrypted end to end. The types are HTTP, HTTPS, SOCKS5 (`socks5://`, which resolves host names on this machine, so those lookups skip the proxy) and SOCKS5 with proxy DNS (`socks5h://`, which leaves them to the proxy).

An invalid host or type is kept as an error that every request returns, so a typo can't send traffic around the proxy. Type *None* leaves reqwest and yt-dlp to their defaults, the `HTTP_PROXY`-style environment variables. A running download keeps its yt-dlp process and open connections until it stops.

## Logins and headers

`Access` holds a download's username, password, referer, cookie and user agent (stored per download, edited in its details window). Every request for the download, the probe and each part, gets:

- **Login** as HTTP Basic, taken from the first that exists: the download's own username, the link's `user:pass@`, or a saved login from Settings → *Logins* whose site is the link's host or a parent of it (`example.com` covers `cdn.example.com`). `set_site_logins` stores the saved list at launch and on every change.
- `Referer`, `Cookie` and `User-Agent` headers for fields that aren't empty; the user agent replaces Syphr's own.

yt-dlp gets the same as `--username`, `--password`, `--referer` and `--add-header Cookie:…` / `--add-header User-Agent:…`. FTP uses the login, or `anonymous` without one.

Passwords are stored as plain text, in `downloads.db` and `settings.json`, like the proxy password. They're also visible in yt-dlp's command line to other processes on the machine.

## Limits

- **Speed.** A token bucket (`Bucket`) caps bytes per second for everything that draws from it; each part draws for every chunk it writes. Each download has its own bucket (`set_limit`), and one more is shared by all direct downloads (`set_total_limit`). A bucket holds at most a quarter second of bytes and may be overdrawn by one chunk; the next taker waits off the debt in steps of at most 250 ms, so a changed limit applies within a quarter second. yt-dlp can't draw from a bucket, so it gets `-r` with the smaller of its own limit and the total, fixed when it starts.
- **Connections per server.** `set_per_server_limit` caps open part connections to one host and port across all direct downloads (0 = none). A part that finds no free connection logs that it's waiting and checks again four times a second. Probes and yt-dlp aren't counted.

## Direct downloads

### 1. Inspect

`inspect(url, access)` sends `GET` with `Range: bytes=0-0`:

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

The layout isn't fixed. While parts run, whenever fewer than `fragments` are running (a part finished, or a resume found fewer unfinished parts), `split_largest` takes the part with the most bytes left and, if that's over 1 MiB, moves the second half of its rest to a new part, which starts at once. So the end of a file runs on as many connections as the start. A new part is only made while the server has a free connection (see [Limits](#limits)).

A part's `end` is behind a lock: the split moves it down while holding the lock, and the part's task holds it while it writes a chunk and counts it. The task cuts each chunk at the current end and stops reading once it reaches it, so no byte is written by two parts, and the counters always match the file. Unit tests check that splits keep every byte covered once; an offline test downloads from a local server where one connection crawls and checks that the others take over its rest.

### 3. Fetch

All bytes go into `<name>.part` in Syphr's data folder, under `partial/<download id>/`, so two downloads of the same link never share parts. Each unfinished part runs as its own task:

- Ranged: `Range: bytes=<start + done>-<end>`, so a part resumes from its own offset. A server that answers anything other than 206 fails the part.
- Not ranged: the file is truncated and the single part restarts from zero.
- Chunks are written with `pwrite` (`write_all_at`) at the part's offset, so parts never need locking. Each part's `done` counter is updated after every write.
- **Speed limit:** after each chunk, the part draws from the download's bucket and the shared one (see [Limits](#limits)).
- **Retries:** a failed part retries up to 5 times, waiting 2, 4, 8, 16, 16 s. Retries resume from the part's offset.
- A part with a known end that stops short is an error, not a success.

If any part fails all its retries, the job fails with that part's error.

### FTP

`ftp://` links take the same path. `inspect` logs in and asks `SIZE`; a known size means parts and resume. Each part opens its own connection, logs in, sets binary mode, sends `REST <offset>` and `RETR`, and reads until its end, then drops the connection. Without `SIZE`, one connection reads the file from the start. FTP can't use the proxy, so with a proxy set FTP links fail instead of connecting around it. Plain FTP only; FTPS isn't supported. The client is `suppaftp` with only its `tokio` feature, so it has no TLS or native dependencies.

### 4. Finish

When every part is done the file is synced, then `.part` is moved to the final name (renamed, or copied and deleted when the destination is on another volume). If something else took that name meanwhile, the next free name is used.

### Pause and resume

`stop()` aborts the part tasks mid-chunk. Since `done` is only advanced after a write lands, the counters match what's on disk. The next `start()` reuses the same layout, reopens `.part` without truncating (a fresh layout, e.g. after a restart, truncates it instead), and starts only the unfinished parts, splitting them again up to `fragments`. If the `.part` file was deleted while paused, every counter resets to zero.

### Refreshing the address

Links that expire can be replaced: *Refresh address…* in a paused or failed direct download's row menu opens its details window on *Options*, where a new link can be pasted. `SyphrApp::refresh_address` inspects it with the download's `Access` and only switches when it supports ranges and reports the same size; otherwise the address stays and a notification says why. Since parts read `request.url` on every start, the next resume continues the existing layout and `.part` from the new link. Like any resume, this needs the layout from this session; after a restart the download starts over (from the new link).

## yt-dlp downloads

### Choosing formats

`probe(bin, url)` runs `yt-dlp -J --no-playlist` and `parse` reduces the format list to a few choices:

- **One per resolution**, highest first: the best video stream at that height (the preferred codec from Settings, then higher fps, then bigger size).
- Video without audio is paired with the best audio in a compatible container (mp4+m4a, webm+webm), otherwise the best audio overall, merged into mkv.
- Selectors use exact format ids (`137+140/b[height=1080]`), so the file matches the label; the `/b[height=…]` fallback covers formats that vanish between lookup and download.
- **Best quality** copies the top resolution. **Audio only** is the highest-bitrate audio stream.
- Each choice shows its container and estimated size (`MP4 · ~48.2 MB`).

`ytdlp::Options` holds Settings → *YouTube & sites → Formats*: preferred codec, video file type and extra arguments. `set_options` stores them at launch and on every change, the way `set_proxy` does, so they apply to lookups and downloads started afterwards.

### Running

`run_ytdlp`:

1. If the selector merges two streams (`+`), checks that ffmpeg runs, and fails with a pointer to *Install required tools* if not.
2. Runs yt-dlp with:
   - `-f <selector> -N <fragments> --no-playlist`
   - `-r <limit>` when a speed limit is set
   - `--ffmpeg-location` when ffmpeg is a full path
   - `--merge-output-format <type> --remux-video <type>` when a video file type is chosen (not for *Audio only*)
   - the user's extra arguments first, so Syphr's own flags win where they clash
   - a progress template that prints `downloaded total` per line, and `--print after_move:filepath` for the final path
   - output into the staging folder `<data folder>/partial/<download id>/` as `<typed name or title>.<ext>`
3. Reads progress from stdout. Video and audio download one after the other, each counting from zero, so a drop in the counter starts a new base. The last stderr line is kept as the error message.
4. On success, moves the file into the destination folder under a free name and removes the staging folder.

yt-dlp is killed when the job is stopped (`kill_on_drop`). The staging folder name is stable, so yt-dlp's own `.part` files let a paused download resume.

The staging folder exists because yt-dlp skips a file that already exists instead of renaming it.

## Naming

`names.rs` decides every file name:

1. **Server name:** `Content-Disposition` (`filename*=UTF-8''…` wins over `filename=`), else the last URL path segment after redirects (percent-decoded), else `download`. With no extension, one is guessed from `Content-Type`.
2. **Typed name** wins. If it has no extension, the server's is added (`notes` + `report.pdf` → `notes.pdf`).
3. **Sanitized:** `/ \ : NUL` and control characters (plus `< > " | ? *` on Windows) become `_`, leading dots are removed, so a name can't escape the folder or hide the file.
4. **Unique:** `name.zip`, then `name (1).zip`, `name (2).zip`, … skipping names where the file exists (the `.part` lives in the download's own folder, so it never blocks a name).

## Errors

Every error is a `String` meant for the user and shown in the failure notification: HTTP errors, "The server stopped accepting partial requests.", "The connection closed before the part finished.", yt-dlp's last error line, or the missing-ffmpeg message.

## Tests

`cargo test` runs the offline tests: part splitting and re-splitting, naming, format parsing, progress parsing, the ffmpeg check, the speed bucket, login lookup, and whole downloads from local HTTP and FTP servers in the tests (`direct::server`): a crawling part taken over by the others, resuming from a new address, login and headers on every request, the per-server limit across two downloads, and FTP parts with `REST` and resume.

`cargo test -- --ignored` also runs live tests against the network: an 8-part download, stop-and-resume, resuming from a refreshed address, `Content-Disposition` naming, and yt-dlp downloads (needs yt-dlp and ffmpeg installed).
