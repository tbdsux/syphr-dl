use std::{collections::HashMap, path::Path, time::SystemTime};

use rusqlite::{Connection, OptionalExtension, params};

use chrono::{NaiveDate, NaiveTime, Timelike};

use crate::download::{Download, Kind, Source, Status};
use crate::engine::Access;
use crate::queue::{Finish, MAIN, Queue};

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
CREATE TABLE IF NOT EXISTS queues (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    start_minute INTEGER,
    stop_minute INTEGER,
    days INTEGER NOT NULL,
    date TEXT,
    max_concurrent INTEGER NOT NULL,
    on_finish TEXT NOT NULL
);
INSERT OR IGNORE INTO queues (id, name, days, max_concurrent, on_finish) VALUES (1, 'Main', 127, 0, 'nothing');
";

/// Lists from before queues: every download joins Main, in the order it was added.
const ADD_QUEUES: &str = "
ALTER TABLE downloads ADD COLUMN queue INTEGER NOT NULL DEFAULT 1;
ALTER TABLE downloads ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
UPDATE downloads SET position = id;
";

/// Columns added since the table was first created; `open` adds the ones an older database lacks.
const ADDED_COLUMNS: &[(&str, &str)] = &[
    ("limit_kbps", "INTEGER"),
    ("username", "TEXT"),
    ("password", "TEXT"),
    ("referer", "TEXT"),
    ("cookie", "TEXT"),
    ("user_agent", "TEXT"),
];

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
        // Checked by column rather than a schema version, so other migrations can land in any order.
        let has_queue: bool =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('downloads') WHERE name = 'queue'", [], |r| {
                r.get(0)
            })?;
        if !has_queue {
            conn.execute_batch(&format!("BEGIN;{ADD_QUEUES}COMMIT;"))?;
        }
        let have: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('downloads')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (name, kind) in ADDED_COLUMNS.iter().filter(|(name, _)| !have.iter().any(|h| h == name)) {
            conn.execute_batch(&format!("ALTER TABLE downloads ADD COLUMN {name} {kind}"))?;
        }
        Ok(Self { conn, saved: HashMap::new() })
    }

    /// Newest first, like the list shows them.
    pub fn load(&mut self) -> rusqlite::Result<Vec<Download>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, url, kind, source, status, total_bytes, received_bytes, folder, format, selector,
                    file_name, path, queue, position, limit_kbps, username, password, referer, cookie, user_agent
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
                    speed_history: Vec::new(),
                    queue: r.get(13)?,
                    position: r.get(14)?,
                    limit_kbps: r.get(15)?,
                    access: Access {
                        username: r.get::<_, Option<String>>(16)?.unwrap_or_default(),
                        password: r.get::<_, Option<String>>(17)?.unwrap_or_default(),
                        referer: r.get::<_, Option<String>>(18)?.unwrap_or_default(),
                        cookie: r.get::<_, Option<String>>(19)?.unwrap_or_default(),
                        user_agent: r.get::<_, Option<String>>(20)?.unwrap_or_default(),
                    },
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
                  file_name, path, queue, position, limit_kbps, username, password, referer, cookie, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
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
                    d.queue,
                    d.position,
                    d.limit_kbps,
                    d.access.username,
                    d.access.password,
                    d.access.referer,
                    d.access.cookie,
                    d.access.user_agent,
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

    /// Main first, then the others in the order they were made. Main is always there.
    pub fn load_queues(&self) -> rusqlite::Result<Vec<Queue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, start_minute, stop_minute, days, date, max_concurrent, on_finish FROM queues ORDER BY id",
        )?;
        let minute = |m: Option<u32>| m.and_then(|m| NaiveTime::from_hms_opt(m / 60, m % 60, 0));
        let queues = stmt
            .query_map([], |r| {
                Ok(Queue {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    start: minute(r.get(2)?),
                    stop: minute(r.get(3)?),
                    days: r.get(4)?,
                    date: r.get::<_, Option<String>>(5)?.and_then(|d| d.parse::<NaiveDate>().ok()),
                    max_concurrent: r.get(6)?,
                    on_finish: Finish::from_key(&r.get::<_, String>(7)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        debug_assert_eq!(queues.first().map(|q| q.id), Some(MAIN));
        Ok(queues)
    }

    /// Replaces the saved queues with `queues`; there are only a handful.
    pub fn save_queues(&mut self, queues: &[Queue]) {
        let save = |conn: &mut Connection| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM queues", [])?;
            let minute = |t: Option<NaiveTime>| t.map(|t| t.hour() * 60 + t.minute());
            for q in queues {
                tx.execute(
                    "INSERT INTO queues (id, name, start_minute, stop_minute, days, date, max_concurrent, on_finish)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        q.id,
                        q.name,
                        minute(q.start),
                        minute(q.stop),
                        q.days,
                        q.date.map(|d| d.to_string()),
                        q.max_concurrent,
                        q.on_finish.key(),
                    ],
                )?;
            }
            tx.commit()
        };
        if let Err(err) = save(&mut self.conn) {
            eprintln!("Couldn't save the queues: {err}");
        }
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
        && (a.queue, a.position) == (b.queue, b.position)
        && (a.limit_kbps, &a.access) == (b.limit_kbps, &b.access)
}

fn status_key(s: Status) -> &'static str {
    match s {
        Status::Downloading => "downloading",
        Status::Paused => "paused",
        Status::Queued => "queued",
        Status::Later => "later",
        Status::Completed => "completed",
        Status::Missing => "missing",
        Status::Failed => "failed",
    }
}

fn status_from(s: &str) -> Status {
    match s {
        "downloading" => Status::Downloading,
        "paused" => Status::Paused,
        "later" => Status::Later,
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
            fragments: vec![(1., 0.5)],
            speed_history: Vec::new(),
            limit_kbps: None,
            access: Default::default(),
            queue: crate::queue::MAIN,
            position: 0,
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
        // Windows won't delete a database that's still open.
        drop((store, reopened));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_lists_join_main_and_queues_round_trip() {
        let dir = std::env::temp_dir().join(format!("syphr-queues-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("downloads.db");
        let _ = std::fs::remove_file(&path);
        // A list saved before queues existed.
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE downloads (id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, kind TEXT NOT NULL,
                source TEXT NOT NULL, status TEXT NOT NULL, total_bytes INTEGER, received_bytes INTEGER NOT NULL,
                folder TEXT NOT NULL, format TEXT, selector TEXT, file_name TEXT, path TEXT);
             INSERT INTO downloads VALUES (7, 'a', 'u', 'video', 'direct', 'queued', NULL, 0, '~', NULL, NULL, NULL, NULL);",
        )
        .unwrap();
        drop(old);

        let mut store = Store::open(&path).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!((loaded[0].queue, loaded[0].position), (MAIN, 7));
        let mut queues = store.load_queues().unwrap();
        assert_eq!(queues, [Queue::new(MAIN, "Main")]);

        queues.push(Queue {
            start: NaiveTime::from_hms_opt(23, 30, 0),
            date: NaiveDate::from_ymd_opt(2026, 12, 1),
            days: 0b101,
            max_concurrent: 2,
            on_finish: Finish::Sleep,
            ..Queue::new(2, "Night")
        });
        store.save_queues(&queues);
        let mut later = download(8, Status::Later);
        later.queue = 2;
        store.sync(&[later]);
        let mut reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.load_queues().unwrap(), queues);
        let d = &reopened.load().unwrap()[0];
        assert_eq!((d.id, d.status, d.queue), (8, Status::Later, 2));
        drop((store, reopened));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adds_new_columns_to_an_old_database() {
        let dir = std::env::temp_dir().join(format!("syphr-store-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("downloads.db");
        let _ = std::fs::remove_file(&path);
        // The table as the first release made it.
        let old = Connection::open(&path).unwrap();
        old.execute_batch(SCHEMA).unwrap();
        old.execute(
            "INSERT INTO downloads (id, name, url, kind, source, status, received_bytes, folder)
             VALUES (1, 'a.zip', 'https://a.com/a.zip', 'archive', 'direct', 'paused', 5, '~/Downloads')",
            [],
        )
        .unwrap();
        drop(old);

        let mut store = Store::open(&path).unwrap();
        let mut list = store.load().unwrap();
        assert_eq!((list[0].id, list[0].status, list[0].limit_kbps), (1, Status::Paused, None));
        list[0].limit_kbps = Some(250);
        list[0].access.username = "me".into();
        list[0].access.cookie = "a=1".into();
        store.sync(&list);
        drop(store);
        let loaded = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(loaded[0].limit_kbps, Some(250));
        assert_eq!(loaded[0].access, Access { username: "me".into(), cookie: "a=1".into(), ..Default::default() });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
