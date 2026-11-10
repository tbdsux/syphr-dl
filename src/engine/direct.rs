//! Direct links: one probe, then parallel byte-range parts written into a `.part` file.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering::Relaxed},
    time::Duration,
};

use reqwest::{StatusCode, header};
use tokio::task::JoinSet;

use super::names::{chosen_name, file_name_of, move_file, part_path, response_file_name, unique_path};
use super::{Access, Layout, Part, Request, ServerSlot, Shared, TOTAL_LIMIT, err};

const MIN_PART_BYTES: u64 = 1024 * 1024;
const RETRIES: u32 = 5;

pub(super) async fn run_direct(request: &Request, shared: &Arc<Shared>) -> Result<PathBuf, String> {
    let fresh = shared.layout.get().is_none();
    let layout = match shared.layout.get() {
        Some(layout) => layout,
        None => {
            let layout = Arc::new(probe(request, shared).await?);
            shared.layout.get_or_init(|| layout)
        }
    };
    let part_path = part_path(request.id, &layout.path);
    if !part_path.exists() {
        // The partial file was removed while paused, so earlier progress is gone.
        layout.parts().iter().for_each(|p| p.done.store(0, Relaxed));
    }
    let part_dir = part_path.parent().expect("part files live in a folder");
    fs::create_dir_all(part_dir).map_err(err)?;
    // A fresh layout (first start, or after a restart) knows of no progress, so stale bytes go.
    let file = Arc::new(OpenOptions::new().create(true).write(true).truncate(fresh).open(&part_path).map_err(err)?);

    let connections = request.fragments.max(1) as usize;
    let mut set = JoinSet::new();
    let spawn = |set: &mut JoinSet<_>, i: usize, part: Arc<Part>| {
        let (request, file, ranged, shared) = (request.clone(), file.clone(), layout.ranged, shared.clone());
        set.spawn(async move { fetch_part_with_retries(&request, &file, ranged, i, &part, &shared).await });
    };
    for (i, part) in layout.parts().into_iter().enumerate() {
        if !layout.ranged || part.left() > 0 {
            spawn(&mut set, i, part);
        }
    }
    loop {
        // Free connections take half of the biggest rest, so the end of the file doesn't run on one connection.
        // A new part only helps if its server takes another connection now.
        while set.len() < connections
            && ServerSlot::has_room(&request.url)
            && let Some((from, i, part)) = layout.split_largest()
        {
            shared.log(format!("Part {}: takes bytes {}–{} from part {}", i + 1, part.start, part.end(), from + 1));
            spawn(&mut set, i, part);
        }
        match set.join_next().await {
            Some(joined) => joined.map_err(err)??,
            None => break,
        }
    }
    file.sync_all().map_err(err)?;
    drop(file);

    // Something may have taken the name while this was downloading.
    let path = if layout.path.exists() {
        unique_path(&request.folder, &file_name_of(&layout.path))
    } else {
        layout.path.clone()
    };
    move_file(&part_path, &path).map_err(err)?;
    let _ = fs::remove_dir_all(part_dir);
    Ok(path)
}

/// What a direct link serves, from `inspect`.
#[derive(Clone, Debug)]
pub struct LinkInfo {
    pub name: String,
    pub total: Option<u64>,
    pub ranged: bool,
    pub content_type: Option<String>,
    /// The request and what came back, for the download's log.
    pub summary: String,
}

/// One tiny ranged request tells us the size, the name, and whether parts are possible.
pub async fn inspect(url: &str, access: &Access) -> Result<LinkInfo, String> {
    let resp = access
        .get(url)?
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(err)?
        .error_for_status()
        .map_err(err)?;
    let ranged = resp.status() == StatusCode::PARTIAL_CONTENT;
    let total = if ranged {
        resp.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<u64>().ok())
    } else {
        resp.content_length()
    };
    let ranged = ranged && total.is_some();
    let name = response_file_name(&resp);
    let content_type: Option<String> =
        resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(Into::into);
    let mut summary = format!("→ {}", resp.status());
    if resp.url().as_str() != url {
        summary += &format!(" from {}", resp.url());
    }
    summary += &format!(
        " · {} · {} · {}",
        total.map(crate::download::format_bytes).unwrap_or_else(|| "size unknown".into()),
        content_type.as_deref().unwrap_or("no type"),
        if ranged { "ranges supported" } else { "no ranges" },
    );
    Ok(LinkInfo { name, total, ranged, content_type, summary })
}

async fn probe(request: &Request, shared: &Shared) -> Result<Layout, String> {
    shared.log(format!("GET {} (Range: bytes=0-0)", request.url));
    let LinkInfo { name, total, ranged, summary, .. } = inspect(&request.url, &request.access).await?;
    shared.log(summary);
    let name = chosen_name(request.file_name.as_deref(), name);

    fs::create_dir_all(&request.folder).map_err(|e| format!("Can't use {}: {e}", request.folder.display()))?;
    let path = unique_path(&request.folder, &name);
    let name = file_name_of(&path);
    let parts = match total {
        Some(total) if ranged => split(total, request.fragments),
        Some(total) => vec![Part::new(0, total.saturating_sub(1))],
        None => vec![Part::new(0, u64::MAX)],
    };
    shared.total.store(total.unwrap_or(0), Relaxed);
    shared.log(match parts.len() {
        1 => format!("Saving to {}", path.display()),
        n => format!("Saving to {} over {n} connections", path.display()),
    });
    *shared.name.lock().unwrap() = Some(name);
    Ok(Layout { path, ranged, parts: Mutex::new(parts.into_iter().map(Arc::new).collect()) })
}

fn split(total: u64, max: u64) -> Vec<Part> {
    let count = (total / MIN_PART_BYTES).clamp(1, max.max(1));
    let size = total.div_ceil(count);
    (0..count)
        .map(|i| i * size)
        .take_while(|start| *start < total)
        .map(|start| Part::new(start, (start + size).min(total) - 1))
        .collect()
}

impl Layout {
    /// Moves the second half of the largest unfinished range to a new part, if that range is over the minimum.
    /// Returns the index of the part it came from, the new part's index, and the new part.
    fn split_largest(&self) -> Option<(usize, usize, Arc<Part>)> {
        if !self.ranged {
            return None;
        }
        let mut parts = self.parts.lock().unwrap();
        let (from, part) = parts.iter().enumerate().max_by_key(|(_, p)| p.left())?;
        let part = part.clone();
        // Locked so the part's task can't write past the new end in the meantime.
        let mut end = part.end.lock().unwrap();
        let next = part.start + part.done.load(Relaxed);
        let left = (*end + 1).saturating_sub(next);
        if left <= MIN_PART_BYTES {
            return None;
        }
        let middle = next + left / 2;
        let new = Arc::new(Part::new(middle, *end));
        *end = middle - 1;
        drop(end);
        parts.push(new.clone());
        Some((from, parts.len() - 1, new))
    }
}

async fn fetch_part_with_retries(
    request: &Request,
    file: &File,
    ranged: bool,
    i: usize,
    part: &Part,
    shared: &Shared,
) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        match fetch_part(request, file, ranged, i, part, shared).await {
            Ok(()) => {
                shared.log(format!("Part {}: done", i + 1));
                return Ok(());
            }
            Err(e) if attempt < RETRIES => {
                attempt += 1;
                shared.log(format!("Part {} failed ({e}), retry {attempt} of {RETRIES}", i + 1));
                tokio::time::sleep(Duration::from_secs(1 << attempt.min(4))).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn fetch_part(
    request: &Request,
    file: &File,
    ranged: bool,
    i: usize,
    part: &Part,
    shared: &Shared,
) -> Result<(), String> {
    let _slot = match ServerSlot::try_take(&request.url) {
        Some(slot) => slot,
        None => {
            shared.log(format!("Part {}: waiting for a free connection to the server", i + 1));
            ServerSlot::take(&request.url).await
        }
    };
    if !ranged {
        // No ranges: the only way to continue is from the start.
        part.done.store(0, Relaxed);
        file.set_len(0).map_err(err)?;
    }
    let mut req = request.access.get(&request.url)?;
    let range = if ranged {
        let range = format!("bytes={}-{}", part.start + part.done.load(Relaxed), part.end());
        req = req.header(header::RANGE, &range);
        format!("Range: {range}")
    } else {
        "no range, from the start".into()
    };
    let resp = req.send().await.map_err(err)?;
    shared.log(format!("Part {}: GET ({range}) → {}", i + 1, resp.status()));
    let mut resp = resp.error_for_status().map_err(err)?;
    if ranged && resp.status() != StatusCode::PARTIAL_CONTENT {
        return Err("The server stopped accepting partial requests.".into());
    }

    while let Some(chunk) = resp.chunk().await.map_err(err)? {
        let wrote = {
            let end = part.end.lock().unwrap();
            let done = part.done.load(Relaxed);
            // A split may have moved the end below what this response still carries.
            let room = (*end - part.start).saturating_add(1).saturating_sub(done);
            let chunk = &chunk[..(chunk.len() as u64).min(room) as usize];
            write_at(file, chunk, part.start + done).map_err(err)?;
            part.done.fetch_add(chunk.len() as u64, Relaxed);
            chunk.len() as u64
        };
        shared.limit.take(wrote).await;
        TOTAL_LIMIT.take(wrote).await;
        if part.left() == 0 {
            break;
        }
    }
    // An unknown size ends when the server closes; a known one must be complete.
    if part.end() != u64::MAX && part.left() > 0 {
        return Err("The connection closed before the part finished.".into());
    }
    Ok(())
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

/// `seek_write` may write less than asked, so loop like `write_all_at` does.
#[cfg(windows)]
fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => (buf, offset) = (&buf[n..], offset + n as u64),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_cover_every_byte_once() {
        for (total, max) in [
            (1, 8),
            (999, 8),
            (MIN_PART_BYTES * 3 + 7, 8),
            (MIN_PART_BYTES * 100, 8),
            (MIN_PART_BYTES * 100, 1),
            (MIN_PART_BYTES * 100, 0),
        ] {
            let parts = split(total, max);
            assert!(parts.len() as u64 <= max.max(1));
            assert_eq!(parts[0].start, 0);
            assert_eq!(parts.last().unwrap().end(), total - 1);
            assert!(parts.windows(2).all(|w| w[1].start == w[0].end() + 1));
        }
    }

    /// Parts in file order must tile 0..total, and no part may count more than its range.
    fn assert_tiles(layout: &Layout, total: u64) {
        let mut parts = layout.parts();
        parts.sort_by_key(|p| p.start);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts.last().unwrap().end(), total - 1);
        assert!(parts.windows(2).all(|w| w[1].start == w[0].end() + 1));
        assert!(parts.iter().all(|p| p.done.load(Relaxed) <= p.len()));
    }

    #[test]
    fn resplits_cover_every_byte_once() {
        let total = MIN_PART_BYTES * 20 + 13;
        let layout = Layout {
            path: PathBuf::new(),
            ranged: true,
            parts: Mutex::new(split(total, 4).into_iter().map(Arc::new).collect()),
        };
        // Part 1 finishes, the others are partway through.
        let parts = layout.parts();
        parts[0].done.store(parts[0].len(), Relaxed);
        parts[1].done.store(MIN_PART_BYTES, Relaxed);
        parts[2].done.store(7, Relaxed);
        let (from, i, new) = layout.split_largest().unwrap();
        // Part 4 has the most left; it keeps the first half of its rest.
        assert_eq!((from, i), (3, 4));
        assert_eq!(new.start, parts[3].end() + 1);
        assert_eq!(new.end(), total - 1);
        assert_tiles(&layout, total);

        // Keep finishing parts and splitting until nothing is big enough to split.
        let mut rounds = 0;
        while let Some((_, _, new)) = layout.split_largest() {
            assert!(new.len() >= MIN_PART_BYTES / 2);
            assert_tiles(&layout, total);
            for p in layout.parts().iter().step_by(2) {
                p.done.store(p.len() / 2, Relaxed);
            }
            rounds += 1;
        }
        assert!(rounds > 0);
        assert!(layout.parts().iter().all(|p| p.left() <= MIN_PART_BYTES));
        assert_tiles(&layout, total);

        let single =
            Layout { path: PathBuf::new(), ranged: false, parts: Mutex::new(vec![Arc::new(Part::new(0, total))]) };
        assert!(single.split_largest().is_none(), "no ranges, no splits");
    }
}

/// A local server with byte ranges, so whole downloads run offline.
#[cfg(test)]
pub(super) mod server {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        time::Duration,
    };

    pub fn byte(i: u64) -> u8 {
        (i % 251) as u8
    }

    pub struct Served {
        pub url: String,
        /// Each request's headers.
        pub seen: Arc<Mutex<Vec<String>>>,
    }

    /// Serves `total` bytes of `byte(i)` with ranges. `slow` gets the first connection that starts at that offset
    /// (other than the one-byte probe) throttled to about 1 MiB/s; `u64::MAX` throttles every one. Each connection
    /// closes after one request.
    pub fn serve(total: u64, slow: Option<u64>) -> Served {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut slow = slow;
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut headers = Vec::new();
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    headers.push(std::mem::take(&mut line).trim().to_string());
                }
                let range =
                    headers.iter().find_map(|h| h.to_lowercase().strip_prefix("range: bytes=").map(String::from));
                log.lock().unwrap().push(headers.join("\n"));
                let (from, to) = match range.as_deref().and_then(|r| r.split_once('-')) {
                    Some((a, b)) => (a.parse().unwrap(), b.parse().unwrap_or(total - 1).min(total - 1)),
                    None => (0, total - 1),
                };
                // Not the one-byte probe.
                let throttle = (slow == Some(from) || slow == Some(u64::MAX)) && to > from;
                if throttle && slow != Some(u64::MAX) {
                    slow = None;
                }
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let status = if range.is_some() { "206 Partial Content" } else { "200 OK" };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{to}/{total}\r\n\
                         Connection: close\r\n\r\n",
                        to - from + 1
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    let body: Vec<u8> = (from..=to).map(byte).collect();
                    for chunk in body.chunks(64 * 1024) {
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        if throttle {
                            std::thread::sleep(Duration::from_millis(60));
                        }
                    }
                });
            }
        });
        Served { url, seen }
    }
}
