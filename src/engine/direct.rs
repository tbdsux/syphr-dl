//! Direct links: one probe, then parallel byte-range parts written into a `.part` file.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use reqwest::{StatusCode, header};
use tokio::task::JoinSet;

use super::names::{chosen_name, file_name_of, move_file, part_path, response_file_name, unique_path};
use super::{CLIENT, Layout, Part, Request, Shared, err};

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
        layout.parts.iter().for_each(|p| p.done.store(0, Relaxed));
    }
    let part_dir = part_path.parent().expect("part files live in a folder");
    fs::create_dir_all(part_dir).map_err(err)?;
    // A fresh layout (first start, or after a restart) knows of no progress, so stale bytes go.
    let file = Arc::new(OpenOptions::new().create(true).write(true).truncate(fresh).open(&part_path).map_err(err)?);

    let pending: Vec<usize> = (0..layout.parts.len())
        .filter(|&i| !layout.ranged || layout.parts[i].done.load(Relaxed) < layout.parts[i].len())
        .collect();
    let limit = request.limit_bps / pending.len().max(1) as u64;
    let mut set = JoinSet::new();
    for i in pending {
        let (url, file, layout, shared) = (request.url.clone(), file.clone(), layout.clone(), shared.clone());
        set.spawn(async move { fetch_part_with_retries(&url, &file, &layout, i, limit, &shared).await });
    }
    while let Some(joined) = set.join_next().await {
        joined.map_err(err)??;
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
pub async fn inspect(url: &str) -> Result<LinkInfo, String> {
    let resp = CLIENT
        .get(url)
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
    let LinkInfo { name, total, ranged, summary, .. } = inspect(&request.url).await?;
    shared.log(summary);
    let name = chosen_name(request.file_name.as_deref(), name);

    fs::create_dir_all(&request.folder).map_err(|e| format!("Can't use {}: {e}", request.folder.display()))?;
    let path = unique_path(&request.folder, &name);
    let name = file_name_of(&path);
    let parts = match total {
        Some(total) if ranged => split(total, request.fragments),
        Some(total) => vec![Part { start: 0, end: total.saturating_sub(1), done: AtomicU64::new(0) }],
        None => vec![Part { start: 0, end: u64::MAX, done: AtomicU64::new(0) }],
    };
    shared.total.store(total.unwrap_or(0), Relaxed);
    shared.log(match parts.len() {
        1 => format!("Saving to {}", path.display()),
        n => format!("Saving to {} over {n} connections", path.display()),
    });
    *shared.name.lock().unwrap() = Some(name);
    Ok(Layout { path, ranged, parts })
}

fn split(total: u64, max: u64) -> Vec<Part> {
    let count = (total / MIN_PART_BYTES).clamp(1, max.max(1));
    let size = total.div_ceil(count);
    (0..count)
        .map(|i| i * size)
        .take_while(|start| *start < total)
        .map(|start| Part { start, end: (start + size).min(total) - 1, done: AtomicU64::new(0) })
        .collect()
}

async fn fetch_part_with_retries(
    url: &str,
    file: &File,
    layout: &Layout,
    i: usize,
    limit: u64,
    shared: &Shared,
) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        match fetch_part(url, file, layout, i, limit, shared).await {
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
    url: &str,
    file: &File,
    layout: &Layout,
    i: usize,
    limit: u64,
    shared: &Shared,
) -> Result<(), String> {
    let part = &layout.parts[i];
    let mut req = CLIENT.get(url);
    let range = if layout.ranged {
        let range = format!("bytes={}-{}", part.start + part.done.load(Relaxed), part.end);
        req = req.header(header::RANGE, &range);
        format!("Range: {range}")
    } else {
        // No ranges: the only way to continue is from the start.
        part.done.store(0, Relaxed);
        file.set_len(0).map_err(err)?;
        "no range, from the start".into()
    };
    let resp = req.send().await.map_err(err)?;
    shared.log(format!("Part {}: GET ({range}) → {}", i + 1, resp.status()));
    let mut resp = resp.error_for_status().map_err(err)?;
    if layout.ranged && resp.status() != StatusCode::PARTIAL_CONTENT {
        return Err("The server stopped accepting partial requests.".into());
    }

    let started = Instant::now();
    let mut got = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(err)? {
        let done = part.done.load(Relaxed);
        let room = part.len() - done;
        let chunk = &chunk[..(chunk.len() as u64).min(room) as usize];
        write_at(file, chunk, part.start + done).map_err(err)?;
        part.done.fetch_add(chunk.len() as u64, Relaxed);
        got += chunk.len() as u64;
        if limit > 0 {
            let due = Duration::from_secs_f64(got as f64 / limit as f64);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                tokio::time::sleep(wait).await;
            }
        }
        if part.done.load(Relaxed) >= part.len() {
            break;
        }
    }
    // An unknown size ends when the server closes; a known one must be complete.
    if part.end != u64::MAX && part.done.load(Relaxed) < part.len() {
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
            assert_eq!(parts.last().unwrap().end, total - 1);
            assert!(parts.windows(2).all(|w| w[1].start == w[0].end + 1));
        }
    }
}
