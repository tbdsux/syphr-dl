//! Runs downloads off the UI thread: direct links in parallel byte-range parts (like IDM), pages through yt-dlp.
//! The app polls each `Job` for progress; a job's parts survive a pause, so resuming continues where it stopped.

mod direct;
mod names;
pub mod ytdlp;

use std::{
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use reqwest::{Client, Proxy};
use tokio::task::JoinHandle;

use direct::run_direct;
pub use direct::{LinkInfo, inspect};
use ytdlp::run_ytdlp;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("tokio runtime")
});

/// The client and proxy for new connections. A bad proxy is kept as an error, so nothing quietly connects without it.
static NETWORK: LazyLock<RwLock<Result<(Client, String), String>>> = LazyLock::new(|| RwLock::new(network("")));

fn network(proxy: &str) -> Result<(Client, String), String> {
    let mut builder = Client::builder()
        .user_agent(concat!("Syphr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30));
    if !proxy.is_empty() {
        builder = builder.proxy(Proxy::all(proxy).map_err(err)?);
    }
    Ok((builder.build().map_err(err)?, proxy.to_string()))
}

/// Applies to connections made from now on. Empty uses the system's `HTTP_PROXY`-style variables, if any.
pub fn set_proxy(proxy: Result<String, String>) {
    *NETWORK.write().unwrap() = proxy.and_then(|proxy| network(&proxy));
}

pub(crate) fn client() -> Result<Client, String> {
    NETWORK.read().unwrap().as_ref().map(|(client, _)| client.clone()).map_err(Clone::clone)
}

/// The proxy to hand to yt-dlp; None when none is set.
pub(crate) fn proxy() -> Result<Option<String>, String> {
    NETWORK
        .read()
        .unwrap()
        .as_ref()
        .map(|(_, proxy)| Some(proxy.clone()).filter(|p| !p.is_empty()))
        .map_err(Clone::clone)
}

/// Caps bytes per second across everything that takes from it; 0 means no cap. Changes apply within a quarter second.
#[derive(Default)]
struct Bucket {
    bps: AtomicU64,
    /// When it was last topped up, and the bytes it held then; negative while a taker has overdrawn it.
    state: Mutex<Option<(Instant, f64)>>,
}

impl Bucket {
    const fn new() -> Self {
        Self { bps: AtomicU64::new(0), state: Mutex::new(None) }
    }

    fn set(&self, bps: u64) {
        self.bps.store(bps, Relaxed);
    }

    /// Waits until the bucket isn't overdrawn, then takes `bytes`, which may overdraw it for the next taker.
    async fn take(&self, bytes: u64) {
        loop {
            let wait = {
                let bps = self.bps.load(Relaxed) as f64;
                if bps == 0. {
                    return;
                }
                let mut state = self.state.lock().unwrap();
                let now = Instant::now();
                let (at, held) = state.unwrap_or((now, 0.));
                // Holds at most a quarter second's worth, so an idle bucket can't burst far over the cap.
                let held = (held + now.duration_since(at).as_secs_f64() * bps).min(bps / 4.);
                if held >= 0. {
                    *state = Some((now, held - bytes as f64));
                    return;
                }
                *state = Some((now, held));
                Duration::from_secs_f64((-held / bps).min(0.25))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// Shared by every direct download.
static TOTAL_LIMIT: Bucket = Bucket::new();

/// Bytes per second for all direct downloads together; 0 means no limit. Applies to running downloads too.
pub fn set_total_limit(bps: u64) {
    TOTAL_LIMIT.set(bps);
}

/// Connections allowed to one server (host and port) across all direct downloads; 0 means no limit.
static PER_SERVER: AtomicU64 = AtomicU64::new(0);
/// Open part connections by server.
static SERVERS: LazyLock<Mutex<std::collections::HashMap<String, u64>>> = LazyLock::new(Default::default);

/// Applies to connections opened from now on; open ones keep going.
pub fn set_per_server_limit(connections: u64) {
    PER_SERVER.store(connections, Relaxed);
}

/// A connection counted against its server until dropped.
struct ServerSlot(String);

impl ServerSlot {
    fn server(url: &str) -> String {
        reqwest::Url::parse(url)
            .ok()
            .and_then(|u| Some(format!("{}:{}", u.host_str()?, u.port_or_known_default()?)))
            .unwrap_or_default()
    }

    fn has_room(url: &str) -> bool {
        let limit = PER_SERVER.load(Relaxed);
        limit == 0 || SERVERS.lock().unwrap().get(&Self::server(url)).copied().unwrap_or(0) < limit
    }

    fn try_take(url: &str) -> Option<Self> {
        let server = Self::server(url);
        let limit = PER_SERVER.load(Relaxed);
        let mut servers = SERVERS.lock().unwrap();
        let open = servers.entry(server.clone()).or_default();
        (limit == 0 || *open < limit).then(|| {
            *open += 1;
            Self(server)
        })
    }

    /// shortcut: polls four times a second instead of queueing waiters; fine for a handful of connections.
    async fn take(url: &str) -> Self {
        loop {
            if let Some(slot) = Self::try_take(url) {
                return slot;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for ServerSlot {
    fn drop(&mut self) {
        let mut servers = SERVERS.lock().unwrap();
        if let Some(open) = servers.get_mut(&self.0) {
            *open -= 1;
            if *open == 0 {
                servers.remove(&self.0);
            }
        }
    }
}

/// Saved logins as (site, username, password); a site also covers its subdomains.
static SITE_LOGINS: RwLock<Vec<(String, String, String)>> = RwLock::new(Vec::new());

/// Applies to requests made from now on, like `set_proxy`.
pub fn set_site_logins(logins: Vec<(String, String, String)>) {
    *SITE_LOGINS.write().unwrap() = logins;
}

/// A download's login and extra headers, sent with the probe and every part request. Empty fields aren't sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub username: String,
    pub password: String,
    pub referer: String,
    pub cookie: String,
    pub user_agent: String,
}

impl Access {
    /// The download's own login, else the one in the link (`user:pass@`), else one saved for the link's site.
    fn login(&self, url: &str) -> Option<(String, String)> {
        if !self.username.is_empty() {
            return Some((self.username.clone(), self.password.clone()));
        }
        let url = reqwest::Url::parse(url).ok()?;
        if !url.username().is_empty() {
            let password = url.password().map(names::percent_decode).unwrap_or_default();
            return Some((names::percent_decode(url.username()), password));
        }
        let host = url.host_str()?.to_ascii_lowercase();
        SITE_LOGINS
            .read()
            .unwrap()
            .iter()
            .find(|(site, ..)| {
                let site = site_host(site);
                !site.is_empty() && (host == site || host.ends_with(&format!(".{site}")))
            })
            .map(|(_, username, password)| (username.clone(), password.clone()))
    }

    /// A GET with the login as HTTP Basic and the extra headers.
    fn get(&self, url: &str) -> Result<reqwest::RequestBuilder, String> {
        let mut req = client()?.get(url);
        if let Some((username, password)) = self.login(url) {
            req = req.basic_auth(username, Some(password));
        }
        for (name, value) in [
            (reqwest::header::REFERER, &self.referer),
            (reqwest::header::COOKIE, &self.cookie),
            (reqwest::header::USER_AGENT, &self.user_agent),
        ] {
            if !value.trim().is_empty() {
                req = req.header(name, value.trim());
            }
        }
        Ok(req)
    }

    /// The login and headers as yt-dlp options.
    fn ytdlp_args(&self, url: &str) -> Vec<String> {
        let mut args = Vec::new();
        if let Some((username, password)) = self.login(url) {
            args.extend(["--username".into(), username, "--password".into(), password]);
        }
        if !self.referer.trim().is_empty() {
            args.extend(["--referer".into(), self.referer.trim().into()]);
        }
        for (name, value) in [("Cookie", &self.cookie), ("User-Agent", &self.user_agent)] {
            if !value.trim().is_empty() {
                args.extend(["--add-header".into(), format!("{name}:{}", value.trim())]);
            }
        }
        args
    }
}

/// `example.com` from a saved site, even if it was pasted as a link.
fn site_host(site: &str) -> String {
    let site = site.trim().to_ascii_lowercase();
    let site = site.split_once("://").map_or(site.as_str(), |(_, rest)| rest);
    let site = site.split(['/', '?', '#']).next().unwrap_or_default();
    site.rsplit_once('@').map_or(site, |(_, host)| host).trim_start_matches("*.").to_string()
}

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
    pub access: Access,
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
    /// This download's own speed limit, on top of `TOTAL_LIMIT`.
    limit: Bucket,
}

impl Shared {
    fn log(&self, line: impl Into<String>) {
        self.log.lock().unwrap().push(line.into());
    }
}

struct Layout {
    path: PathBuf,
    ranged: bool,
    /// Grows while downloading: a connection that finishes early takes over half of another part's rest.
    parts: Mutex<Vec<Arc<Part>>>,
}

impl Layout {
    fn parts(&self) -> Vec<Arc<Part>> {
        self.parts.lock().unwrap().clone()
    }
}

struct Part {
    start: u64,
    /// Inclusive; u64::MAX when the size is unknown. A split moves it down, so it stays locked while a chunk is
    /// written and counted.
    end: Mutex<u64>,
    done: AtomicU64,
}

impl Part {
    fn new(start: u64, end: u64) -> Self {
        Self { start, end: Mutex::new(end), done: AtomicU64::new(0) }
    }

    fn end(&self) -> u64 {
        *self.end.lock().unwrap()
    }

    fn len(&self) -> u64 {
        self.end().saturating_sub(self.start).saturating_add(1)
    }

    fn left(&self) -> u64 {
        self.len().saturating_sub(self.done.load(Relaxed))
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
        self.set_limit(request.limit_bps);
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

    /// Bytes per second; 0 means unlimited. Applies at once to direct downloads, and on the next start to yt-dlp.
    pub fn set_limit(&self, bps: u64) {
        self.shared.limit.set(bps);
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
            Some(layout) => layout.parts().iter().map(|p| p.done.load(Relaxed)).sum(),
            None => self.shared.received.load(Relaxed),
        }
    }

    pub fn total(&self) -> Option<u64> {
        Some(self.shared.total.load(Relaxed)).filter(|t| *t > 0)
    }

    /// Each part in file order as (share of the file, finished fraction), both 0–1; empty when there is only one
    /// part or yt-dlp runs it.
    pub fn fragments(&self) -> Vec<(f32, f32)> {
        let Some(layout) = self.shared.layout.get() else { return Vec::new() };
        let mut parts = layout.parts();
        let total = self.shared.total.load(Relaxed);
        if parts.len() < 2 || total == 0 {
            return Vec::new();
        }
        parts.sort_by_key(|p| p.start);
        parts.iter().map(|p| (p.len() as f32 / total as f32, p.done.load(Relaxed) as f32 / p.len() as f32)).collect()
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

/// The whole cause chain: reqwest's own message only says the request failed, not that a proxy refused it.
fn err(e: impl std::error::Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        message += &format!(": {cause}");
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
    };

    #[test]
    fn requests_go_through_the_proxy() {
        // socks5 resolves names locally before connecting, so it gets an address; socks5h leaves them to the proxy.
        for (scheme, target, first_byte) in [
            ("http", "http://syphr.invalid/", b'G'),
            ("socks5", "http://127.0.0.1:9/", 5),
            ("socks5h", "http://syphr.invalid/", 5),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let (client, _) = network(&format!("{scheme}://{}", listener.local_addr().unwrap())).unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                tx.send(byte[0]).unwrap();
                // Holds the connection open until the test has its answer.
                let _ = stream.read(&mut byte);
            });
            RUNTIME.spawn(async move { client.get(target).send().await });
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), first_byte, "{scheme}");
        }
    }

    #[test]
    fn explains_a_refused_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 1024]);
            stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").unwrap();
        });
        let (client, _) = network(&proxy).unwrap();
        let sent = RUNTIME.block_on(async move { client.get("https://127.0.0.1:9/").send().await });
        let message = err(sent.unwrap_err());
        assert!(message.to_lowercase().contains("proxy"), "{message}");
    }

    #[test]
    fn sends_proxy_credentials() {
        let settings = |proxy_type: &str, port| crate::ui::settings::AppSettings {
            proxy_type: proxy_type.into(),
            proxy_host: "127.0.0.1".into(),
            proxy_port: port as f64,
            proxy_username: "me".into(),
            proxy_password: "p@ss:w/rd é".into(),
            ..Default::default()
        };
        for proxy_type in ["http", "socks5"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = settings(proxy_type, listener.local_addr().unwrap().port()).proxy_url().unwrap();
            let (client, _) = network(&url).unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                tx.send(buf[..n].to_vec()).unwrap();
                let _ = stream.read(&mut buf);
            });
            RUNTIME.spawn(async move { client.get("http://127.0.0.1:9/").send().await });
            let sent = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if proxy_type == "http" {
                // Base64 of "me:p@ss:w/rd é".
                let text = String::from_utf8_lossy(&sent).to_lowercase();
                assert!(text.contains("proxy-authorization: basic bwu6cebzczp3l3jkimop"), "{text}");
            } else {
                // SOCKS5 greeting: version, method count, then the methods; 2 is username and password.
                assert_eq!(sent[0], 5);
                assert!(sent[2..].contains(&2), "{sent:?}");
            }
        }
    }

    #[test]
    fn bucket_caps_all_takers_together() {
        let bucket = Arc::new(Bucket::new());
        bucket.set(2 * 1024 * 1024);
        let started = Instant::now();
        RUNTIME.block_on(async {
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    let bucket = bucket.clone();
                    RUNTIME.spawn(async move {
                        for _ in 0..16 {
                            bucket.take(16 * 1024).await;
                        }
                    })
                })
                .collect();
            for task in tasks {
                task.await.unwrap();
            }
        });
        // 1 MiB at 2 MiB/s, less the first chunk, which goes straight through.
        let secs = started.elapsed().as_secs_f64();
        assert!((0.4..0.8).contains(&secs), "{secs}");

        // Lifting the cap releases a taker that is waiting.
        let bucket = Arc::new(Bucket::new());
        bucket.set(1);
        let started = Instant::now();
        RUNTIME.block_on(async {
            bucket.take(1024 * 1024).await;
            let waiting = RUNTIME.spawn({
                let bucket = bucket.clone();
                async move { bucket.take(1).await }
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
            bucket.set(0);
            waiting.await.unwrap();
        });
        assert!(started.elapsed() < Duration::from_secs(1));
    }

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
            access: Access::default(),
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
mod offline {
    use super::*;
    use direct::server::{byte, serve};

    pub(super) fn request(id: u64, url: &str) -> Request {
        Request {
            id,
            url: url.into(),
            folder: std::env::temp_dir().join(format!("syphr-offline-{}-{id}", std::process::id())),
            selector: None,
            file_name: None,
            ytdlp_bin: String::new(),
            ffmpeg_bin: String::new(),
            limit_bps: 0,
            fragments: 2,
            access: Access::default(),
        }
    }

    pub(super) fn finish(job: &mut Job) -> Result<PathBuf, String> {
        for _ in 0..600 {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(result) = job.take_result() {
                return result;
            }
        }
        panic!("the download didn't finish")
    }

    pub(super) fn assert_content(path: &std::path::Path, total: u64) {
        let data = std::fs::read(path).unwrap();
        assert_eq!(data.len() as u64, total);
        assert!(data.iter().enumerate().all(|(i, b)| *b == byte(i as u64)), "bytes out of place");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn finished_connections_take_over_the_slow_part() {
        let total = 8 * 1024 * 1024 + 5;
        // Two parts; the second crawls, so the first must take over half of what it has left, more than once.
        let direct::server::Served { url, seen, .. } = serve(total, Some(total.div_ceil(2)));
        let mut job = Job::default();
        job.start(request(u64::MAX - 10, &url));
        let path = finish(&mut job).unwrap();
        assert_content(&path, total);
        let parts = job.shared.layout.get().unwrap().parts().len();
        assert!(parts > 2, "only {parts} parts");
        assert_eq!(seen.lock().unwrap().len(), parts + 1, "one probe and one request per part");
        assert_eq!(job.received(), total);
        assert!(job.fragments().iter().all(|(_, done)| *done == 1.));
    }

    #[test]
    fn sends_login_and_headers_with_every_request() {
        let total = 3 * 1024 * 1024;
        let direct::server::Served { url, seen, .. } = serve(total, None);
        let access = Access {
            username: "me".into(),
            password: "p@ss".into(),
            referer: "https://example.com/page".into(),
            cookie: "session=1".into(),
            user_agent: "Test/1".into(),
        };
        let mut job = Job::default();
        job.start(Request { access, ..request(u64::MAX - 12, &url) });
        assert_content(&finish(&mut job).unwrap(), total);
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 3, "probe and parts: {seen:?}");
        for headers in seen.iter().map(|h| h.to_lowercase()) {
            // Base64 of "me:p@ss".
            assert!(headers.contains("authorization: basic bwu6cebzcw=="), "{headers}");
            assert!(headers.contains("referer: https://example.com/page"), "{headers}");
            assert!(headers.contains("cookie: session=1"), "{headers}");
            assert!(headers.contains("user-agent: test/1"), "{headers}");
        }
    }

    #[test]
    fn limits_connections_per_server() {
        let total = 4 * 1024 * 1024;
        let served = serve(total, Some(u64::MAX));
        // Global, but the other offline tests use their own servers and at most two connections each.
        set_per_server_limit(3);
        let mut jobs: Vec<Job> = (0..2).map(|_| Job::default()).collect();
        for (i, job) in jobs.iter_mut().enumerate() {
            job.start(Request { fragments: 4, ..request(u64::MAX - 20 - i as u64, &served.url) });
        }
        let server = ServerSlot::server(&served.url);
        let (mut peak, mut paths) = (0, Vec::new());
        while paths.len() < 2 {
            std::thread::sleep(Duration::from_millis(5));
            peak = peak.max(SERVERS.lock().unwrap().get(&server).copied().unwrap_or(0));
            paths.extend(jobs.iter_mut().filter_map(|job| job.take_result()).map(Result::unwrap));
        }
        set_per_server_limit(0);
        assert_eq!(peak, 3);
        for path in paths {
            assert_content(&path, total);
        }
        assert!(SERVERS.lock().unwrap().is_empty(), "every slot was given back");
    }

    #[test]
    fn finds_the_login_for_a_link() {
        set_site_logins(vec![("https://Files.Example.com/".into(), "site".into(), "pw".into())]);
        let none = Access::default();
        let own = Access { username: "me".into(), ..Default::default() };
        let login = |a: &Access, url: &str| a.login(url);
        let pair = |u: &str, p: &str| Some((u.to_string(), p.to_string()));
        assert_eq!(login(&own, "https://files.example.com/a"), pair("me", ""));
        assert_eq!(login(&none, "https://u%40x:p%3A1@other.org/a"), pair("u@x", "p:1"));
        assert_eq!(login(&none, "https://files.example.com/a"), pair("site", "pw"));
        assert_eq!(login(&none, "https://cdn.files.example.com/a"), pair("site", "pw"));
        assert_eq!(login(&none, "https://example.com/a"), None);
        assert_eq!(login(&none, "https://notfiles.example.com/a"), None);
        set_site_logins(Vec::new());

        let args = Access { referer: "https://r/".into(), cookie: "a=1".into(), ..own }.ytdlp_args("https://x/");
        assert_eq!(
            args,
            ["--username", "me", "--password", "", "--referer", "https://r/", "--add-header", "Cookie:a=1"]
        );
    }

    #[test]
    fn resumes_from_a_new_address() {
        let total = 6 * 1024 * 1024;
        let old = serve(total, Some(0)).url;
        let mut job = Job::default();
        let request = Request { fragments: 1, ..request(u64::MAX - 11, &old) };
        job.start(request.clone());
        std::thread::sleep(Duration::from_millis(1500));
        job.stop();
        let paused = job.received();
        assert!(paused > 0 && paused < total, "{paused}");

        let direct::server::Served { url: new, seen, .. } = serve(total, None);
        job.start(Request { url: new, ..request });
        let path = finish(&mut job).unwrap();
        assert_content(&path, total);
        let first = seen.lock().unwrap().iter().map(|h| h.to_lowercase()).find(|h| !h.contains("bytes=0-0")).unwrap();
        assert!(first.contains(&format!("range: bytes={paused}-")), "{first}");
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
            access: Access::default(),
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
                    job.shared.layout.get().map(|l| l.parts().len()),
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
            access: Access::default(),
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
    fn resumes_from_a_refreshed_address() {
        let request = Request {
            id: u64::MAX - 4,
            url: "https://proof.ovh.net/files/10Mb.dat".into(),
            folder: std::env::temp_dir().join(format!("syphr-refresh-{}", std::process::id())),
            file_name: None,
            selector: None,
            ytdlp_bin: String::new(),
            ffmpeg_bin: String::new(),
            limit_bps: 2 * 1024 * 1024,
            fragments: 4,
            access: Access::default(),
        };
        let mut job = Job::default();
        job.start(request.clone());
        std::thread::sleep(Duration::from_millis(2000));
        job.stop();
        let paused = job.received();
        assert!(paused > 0 && paused < 10 * 1024 * 1024, "{paused}");
        job.take_log();
        // Another address for the same file.
        let url = "https://proof.ovh.net/files/10Mb.dat?refreshed=1".to_string();
        let info = RUNTIME.block_on(inspect(&url, &Access::default())).unwrap();
        assert_eq!((info.total, info.ranged), (Some(10 * 1024 * 1024), true));
        job.start(Request { url, limit_bps: 0, ..request });
        let path = loop {
            std::thread::sleep(Duration::from_millis(200));
            if let Some(result) = job.take_result() {
                break result.unwrap();
            }
        };
        assert_eq!(fs::metadata(&path).unwrap().len(), 10 * 1024 * 1024);
        let log = job.take_log();
        assert!(!log.iter().any(|l| l.contains("GET (Range: bytes=0-")), "part 1 restarted: {log:?}");
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
