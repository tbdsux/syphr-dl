use std::{collections::HashMap, path::Path, time::SystemTime};

use rusqlite::{Connection, OptionalExtension, params};

use crate::download::{Download, Kind, Source, Status};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS downloads (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL,
    total_bytes INTEGER,
    received_bytes INTEGER NOT NULL,
    folder TEXT NOT NULL,
    format TEXT,
    selector TEXT,
    file_name TEXT,
    path TEXT
);
CREATE TABLE IF NOT EXISTS log (
    id INTEGER PRIMARY KEY,
    download_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS log_download ON log (download_id);
";

/// The download list and each download's log, in `downloads.db` in Syphr's folder.
pub struct Store {
    conn: Connection,
    /// What the database holds, so `sync` only writes rows that changed.
    saved: HashMap<u64, Download>,
}

impl Store {
    /// Falls back to an in-memory database so the app still runs when the file can't be opened.
    pub fn open_default() -> Self {
        let path = crate::ui::settings::data_dir().join("downloads.db");
        let _ = std::fs::create_dir_all(crate::ui::settings::data_dir());
        Self::open(&path).unwrap_or_else(|err| {
            eprintln!("Couldn't open {}, the list won't be kept: {err}", path.display());
            Self::open(Path::new(":memory:")).expect("in-memory database")
        })
    }

    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, saved: HashMap::new() })
    }

    /// Newest first, like the list shows them.
    pub fn load(&mut self) -> rusqlite::Result<Vec<Download>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, url, kind, source, status, total_bytes, received_bytes, folder, format, selector,
                    file_name, path
             FROM downloads ORDER BY id DESC",
        )?;
        let downloads = stmt
            .query_map([], |r| {
                Ok(Download {
                    id: r.get(0)?,
                    name: r.get::<_, String>(1)?.into(),
                    url: r.get::<_, String>(2)?.into(),
                    kind: kind_from(&r.get::<_, String>(3)?),
                    source: if r.get::<_, String>(4)? == "ytdlp" { Source::YtDlp } else { Source::Direct },
                    status: status_from(&r.get::<_, String>(5)?),
                    total_bytes: r.get(6)?,
                    received_bytes: r.get(7)?,
                    bytes_per_sec: 0,
                    folder: r.get::<_, String>(8)?.into(),
                    format: r.get::<_, Option<String>>(9)?.map(Into::into),
                    selector: r.get(10)?,
                    file_name: r.get(11)?,
                    path: r.get::<_, Option<String>>(12)?.map(Into::into),
                    fragments: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        self.saved = downloads.iter().map(|d| (d.id, d.clone())).collect();
        Ok(downloads)
    }

    pub fn next_id(&self) -> u64 {
        let max: Option<u64> =
            self.conn.query_row("SELECT MAX(id) FROM downloads", [], |r| r.get(0)).optional().ok().flatten().flatten();
        max.unwrap_or(0) + 1
    }

    /// Writes rows that changed since the last sync, deletes removed ones with their logs,
    /// and logs status changes.
    pub fn sync(&mut self, downloads: &[Download]) {
        if let Err(err) = self.try_sync(downloads) {
            eprintln!("Couldn't save the download list: {err}");
        }
    }

    fn try_sync(&mut self, downloads: &[Download]) -> rusqlite::Result<()> {
        let changed: Vec<&Download> =
            downloads.iter().filter(|d| self.saved.get(&d.id).is_none_or(|s| !same(s, d))).collect();
        let removed: Vec<u64> =
            self.saved.keys().filter(|id| !downloads.iter().any(|d| d.id == **id)).copied().collect();
        if changed.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for d in &changed {
            tx.execute(
                "INSERT OR REPLACE INTO downloads
                 (id, name, url, kind, source, status, total_bytes, received_bytes, folder, format, selector,
                  file_name, path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    d.id,
                    d.name.as_ref(),
                    d.url.as_ref(),
                    kind_key(d.kind),
                    if d.source == Source::YtDlp { "ytdlp" } else { "direct" },
                    status_key(d.status),
                    d.total_bytes,
                    d.received_bytes,
                    d.folder.as_ref(),
                    d.format.as_deref(),
                    d.selector,
                    d.file_name,
                    d.path.as_ref().map(|p| p.to_string_lossy()),
                ],
            )?;
            let before = self.saved.get(&d.id).map(|s| s.status);
            if before != Some(d.status) {
                let message = match before {
                    None => format!("Added {} ({})", d.url, status_key(d.status)),
                    Some(_) => status_key(d.status).to_string(),
                };
                append(&tx, d.id, &message)?;
            }
        }
        for id in &removed {
            tx.execute("DELETE FROM downloads WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM log WHERE download_id = ?1", [id])?;
        }
        tx.commit()?;
        for d in changed {
            self.saved.insert(d.id, d.clone());
        }
        for id in removed {
            self.saved.remove(&id);
        }
        Ok(())
    }

    pub fn log(&self, id: u64, message: &str) {
        if let Err(err) = append(&self.conn, id, message) {
            eprintln!("Couldn't write the download log: {err}");
        }
    }

    /// Oldest first, as (local "HH:MM:SS", message).
    pub fn log_for(&self, id: u64) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT strftime('%H:%M:%S', at, 'unixepoch', 'localtime'), message
             FROM log WHERE download_id = ?1 ORDER BY id",
        )?;
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }
}

fn append(conn: &Connection, id: u64, message: &str) -> rusqlite::Result<usize> {
    let at = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    conn.execute("INSERT INTO log (download_id, at, message) VALUES (?1, ?2, ?3)", params![id, at, message])
}

/// Live-only fields (speed, part fractions) don't count, so a steady download writes only its byte count.
fn same(a: &Download, b: &Download) -> bool {
    (&a.name, &a.url, a.kind, a.status, a.total_bytes, a.received_bytes, &a.folder, &a.format, &a.path)
        == (&b.name, &b.url, b.kind, b.status, b.total_bytes, b.received_bytes, &b.folder, &b.format, &b.path)
}

fn status_key(s: Status) -> &'static str {
    match s {
        Status::Downloading => "downloading",
        Status::Paused => "paused",
        Status::Queued => "queued",
        Status::Completed => "completed",
        Status::Missing => "missing",
        Status::Failed => "failed",
    }
}

fn status_from(s: &str) -> Status {
    match s {
        "downloading" => Status::Downloading,
        "paused" => Status::Paused,
        "completed" => Status::Completed,
        "missing" => Status::Missing,
        "failed" => Status::Failed,
        _ => Status::Queued,
    }
}

fn kind_key(k: Kind) -> &'static str {
    match k {
        Kind::Video => "video",
        Kind::Music => "music",
        Kind::Document => "document",
        Kind::Program => "program",
        Kind::Archive => "archive",
    }
}

fn kind_from(s: &str) -> Kind {
    match s {
        "video" => Kind::Video,
        "music" => Kind::Music,
        "program" => Kind::Program,
        "archive" => Kind::Archive,
        _ => Kind::Document,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(id: u64, status: Status) -> Download {
        Download {
            id,
            name: format!("{id}.mp4").into(),
            url: "https://a.com/a.mp4".into(),
            kind: Kind::Video,
            source: Source::YtDlp,
            status,
            total_bytes: Some(10),
            received_bytes: 3,
            bytes_per_sec: 99,
            folder: "~/Downloads".into(),
            format: Some("1080p".into()),
            selector: Some("bv*".into()),
            file_name: None,
            path: Some("/tmp/1.mp4".into()),
            fragments: vec![0.5],
        }
    }

    #[test]
    fn round_trip_and_log() {
        let dir = std::env::temp_dir().join(format!("syphr-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("downloads.db");
        let _ = std::fs::remove_file(&path);

        let mut store = Store::open(&path).unwrap();
        let mut list = vec![download(2, Status::Downloading), download(1, Status::Queued)];
        store.sync(&list);
        list[0].status = Status::Completed;
        list[0].bytes_per_sec = 0;
        store.sync(&list);
        store.log(2, "boom");
        list.remove(1);
        store.sync(&list);
        assert_eq!(store.log_for(1).unwrap(), vec![]);
        let messages: Vec<String> = store.log_for(2).unwrap().into_iter().map(|(_, m)| m).collect();
        assert_eq!(messages, ["Added https://a.com/a.mp4 (downloading)", "completed", "boom"]);

        let mut reopened = Store::open(&path).unwrap();
        let loaded = reopened.load().unwrap();
        assert_eq!(loaded.len(), 1);
        let d = &loaded[0];
        assert_eq!((d.id, d.status, d.kind, d.source), (2, Status::Completed, Kind::Video, Source::YtDlp));
        assert_eq!((d.format.as_deref(), d.selector.as_deref(), d.bytes_per_sec), (Some("1080p"), Some("bv*"), 0));
        assert_eq!(reopened.next_id(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
