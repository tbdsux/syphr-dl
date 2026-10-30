//! Runs downloads off the UI thread: direct links in parallel byte-range parts (like IDM), pages through yt-dlp.
//! The app polls each `Job` for progress; a job's parts survive a pause, so resuming continues where it stopped.

mod direct;
mod names;
pub mod ytdlp;

use std::{
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use reqwest::Client;
use tokio::task::JoinHandle;

use direct::run_direct;
pub use direct::{LinkInfo, inspect};
use ytdlp::run_ytdlp;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("tokio runtime")
});

pub(crate) static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent(concat!("Syphr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
});

#[derive(Clone)]
pub struct Request {
    /// The download's id; its unfinished files live under it.
    pub id: u64,
    pub url: String,
    pub folder: PathBuf,
    /// yt-dlp `-f` selector; None downloads the link directly.
    pub selector: Option<String>,
    /// The name the user typed; None uses the server's name or the video title.
    pub file_name: Option<String>,
    pub ytdlp_bin: String,
    pub ffmpeg_bin: String,
    /// Bytes per second; 0 means unlimited.
    pub limit_bps: u64,
    /// Parallel connections; servers that don't support ranges get one.
    pub fragments: u64,
}

#[derive(Default)]
struct Shared {
    /// 0 while unknown.
    total: AtomicU64,
    /// yt-dlp progress; direct downloads sum their parts instead.
    received: AtomicU64,
    name: Mutex<Option<String>>,
    layout: OnceLock<Arc<Layout>>,
    result: Mutex<Option<Result<PathBuf, String>>>,
    /// Lines for the download's log, taken by the app each tick.
    log: Mutex<Vec<String>>,
}

impl Shared {
    fn log(&self, line: impl Into<String>) {
        self.log.lock().unwrap().push(line.into());
    }
}

struct Layout {
    path: PathBuf,
    ranged: bool,
    parts: Vec<Part>,
}

struct Part {
    start: u64,
    /// Inclusive; u64::MAX when the size is unknown.
    end: u64,
    done: AtomicU64,
}

impl Part {
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }
}

#[derive(Default)]
pub struct Job {
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
    last: Option<(Instant, u64)>,
    bps: u64,
}

impl Job {
    pub fn start(&mut self, request: Request) {
        self.stop();
        let shared = self.shared.clone();
        self.task = Some(RUNTIME.spawn(async move {
            let result = match request.selector.clone() {
                Some(selector) => run_ytdlp(&request, &selector, &shared).await,
                None => run_direct(&request, &shared).await,
            };
            *shared.result.lock().unwrap() = Some(result);
        }));
    }

    /// Aborting drops the part tasks and kills yt-dlp; finished parts are kept for the next start.
    pub fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.last = None;
        self.bps = 0;
    }

    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Set once when the run ends on its own (not after `stop`).
    pub fn take_result(&mut self) -> Option<Result<PathBuf, String>> {
        let result = self.shared.result.lock().unwrap().take();
        if result.is_some() {
            self.task = None;
            self.bps = 0;
        }
        result
    }

    pub fn received(&self) -> u64 {
        match self.shared.layout.get() {
            Some(layout) => layout.parts.iter().map(|p| p.done.load(Relaxed)).sum(),
            None => self.shared.received.load(Relaxed),
        }
    }

    pub fn total(&self) -> Option<u64> {
        Some(self.shared.total.load(Relaxed)).filter(|t| *t > 0)
    }

    /// Each part's finished fraction (0–1); empty when there is only one part or yt-dlp runs it.
    pub fn fragments(&self) -> Vec<f32> {
        match self.shared.layout.get() {
            Some(layout) if layout.parts.len() > 1 => {
                layout.parts.iter().map(|p| p.done.load(Relaxed) as f32 / p.len() as f32).collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.log.lock().unwrap())
    }

    /// The file name the server gave, once known.
    pub fn name(&self) -> Option<String> {
        self.shared.name.lock().unwrap().clone()
    }

    /// Smoothed speed; call about once per UI tick.
    pub fn speed(&mut self) -> u64 {
        let now = (Instant::now(), self.received());
        if let Some((at, bytes)) = self.last {
            let secs = now.0.duration_since(at).as_secs_f64();
            if secs > 0. {
                let instant = (now.1.saturating_sub(bytes)) as f64 / secs;
                self.bps = (self.bps as f64 * 0.6 + instant * 0.4) as u64;
            }
        }
        self.last = Some(now);
        self.bps
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stops a removed download and deletes its unfinished files.
pub fn discard(id: u64, job: Option<Job>) {
    RUNTIME.spawn(async move {
        if let Some(task) = job.and_then(|mut j| j.task.take()) {
            task.abort();
            let _ = task.await;
        }
        // A killed yt-dlp can hold its files open a moment longer, which blocks deleting them on Windows.
        for _ in 0..5 {
            match std::fs::remove_dir_all(names::partial_dir(id)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => tokio::time::sleep(Duration::from_secs(1)).await,
                _ => return,
            }
        }
    });
}

/// The folder holding unfinished downloads, and the bytes they take.
pub fn partial_usage() -> (PathBuf, u64) {
    fn size(path: &std::path::Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(path) else { return 0 };
        entries
            .flatten()
            .map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => size(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            })
            .sum()
    }
    let root = names::partial_root();
    let bytes = size(&root);
    (root, bytes)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_formats_need_ffmpeg() {
        let mut job = Job::default();
        job.start(Request {
            id: u64::MAX,
            url: "https://www.youtube.com/watch?v=x".into(),
            folder: std::env::temp_dir(),
            selector: Some("bv*+ba/b".into()),
            file_name: None,
            ytdlp_bin: "yt-dlp".into(),
            ffmpeg_bin: "no-such-ffmpeg".into(),
            limit_bps: 0,
            fragments: 8,
        });
        let result = loop {
            std::thread::sleep(Duration::from_millis(20));
            if let Some(result) = job.take_result() {
                break result;
            }
        };
        assert!(result.unwrap_err().starts_with("ffmpeg is required"));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use names::file_name_of;
    use std::fs;

    fn run(url: &str, selector: Option<&str>) -> (PathBuf, u64) {
        let folder = std::env::temp_dir().join(format!("syphr-live-{}", std::process::id()));
        let mut job = Job::default();
        job.start(Request {
            id: u64::MAX - 1,
            url: url.into(),
            folder,
            selector: selector.map(Into::into),
            file_name: None,
            ytdlp_bin: "yt-dlp".into(),
            ffmpeg_bin: "ffmpeg".into(),
            limit_bps: 0,
            fragments: 8,
        });
        let mut peak = 0;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            peak = peak.max(job.speed());
            if let Some(result) = job.take_result() {
                job.take_log().iter().for_each(|line| eprintln!("log: {line}"));
                let path = result.unwrap();
                eprintln!(
                    "{} {} bytes, parts {:?}, peak {}/s",
                    path.display(),
                    fs::metadata(&path).unwrap().len(),
                    job.shared.layout.get().map(|l| l.parts.len()),
                    crate::download::format_bytes(peak)
                );
                return (path, job.received());
            }
        }
    }

    #[test]
    #[ignore = "network"]
    fn downloads_in_parts() {
        let (path, received) = run("https://proof.ovh.net/files/10Mb.dat", None);
        assert_eq!(fs::metadata(&path).unwrap().len(), 10 * 1024 * 1024);
        assert_eq!(received, 10 * 1024 * 1024);
    }

    #[test]
    #[ignore = "network"]
    fn resumes_after_stop() {
        let request = Request {
            id: u64::MAX - 2,
            url: "https://proof.ovh.net/files/10Mb.dat".into(),
            folder: std::env::temp_dir().join(format!("syphr-resume-{}", std::process::id())),
            file_name: None,
            selector: None,
            ytdlp_bin: String::new(),
            ffmpeg_bin: String::new(),
            limit_bps: 2 * 1024 * 1024,
            fragments: 8,
        };
        let mut job = Job::default();
        job.start(request.clone());
        std::thread::sleep(Duration::from_millis(2000));
        job.stop();
        let paused = job.received();
        assert!(paused > 0 && paused < 10 * 1024 * 1024, "{paused}");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(job.received(), paused, "nothing writes after stop");
        job.start(Request { limit_bps: 0, ..request });
        let path = loop {
            std::thread::sleep(Duration::from_millis(200));
            if let Some(result) = job.take_result() {
                break result.unwrap();
            }
        };
        assert_eq!(fs::metadata(&path).unwrap().len(), 10 * 1024 * 1024);
        assert_eq!(job.received(), 10 * 1024 * 1024);
    }

    #[test]
    #[ignore = "network"]
    fn names_from_content_disposition() {
        let (path, _) = run("https://codeload.github.com/rust-lang/rustlings/zip/refs/heads/main", None);
        assert!(file_name_of(&path).ends_with(".zip"), "{}", path.display());
    }

    #[test]
    #[ignore = "network"]
    fn saved_file_matches_chosen_format() {
        let info = ytdlp::probe("yt-dlp", "https://www.youtube.com/watch?v=jNQXAC9IVRw").unwrap();
        let option = &info.options[1];
        let (path, _) = run("https://www.youtube.com/watch?v=jNQXAC9IVRw", Some(&option.selector));
        let ext = path.extension().unwrap().to_string_lossy().to_uppercase();
        assert!(option.detail.starts_with(&*ext), "{} vs {}", option.detail, path.display());
    }

    #[test]
    #[ignore = "network"]
    fn downloads_with_ytdlp() {
        let (path, received) = run("https://www.youtube.com/watch?v=jNQXAC9IVRw", Some("ba[ext=m4a]/ba"));
        assert!(path.exists() && received > 0, "{}", path.display());
    }
}
