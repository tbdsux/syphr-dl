//! Links handed to Syphr from outside its window: the command line, a second launch, `syphr://` links and
//! clipboard notifications. A second launch forwards its request to the running app and exits.

use std::{
    fs,
    hash::{BuildHasher, RandomState},
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Downloads to add, with how to add them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AddRequest {
    pub urls: Vec<String>,
    /// Save folder; None uses the one in Settings.
    pub dir: Option<String>,
    /// File name, for a single link.
    pub name: Option<String>,
    /// Add without showing the panel.
    pub silent: bool,
    /// Add as `Later`, waiting for its queue to start; implies `silent`.
    pub queue_only: bool,
}

const USAGE: &str = "\
Usage: syphr-dl [LINK...] [--dir FOLDER] [--name FILE] [--silent] [--queue-only]

  LINK            http(s) links, or syphr://add?url=LINK
  --dir FOLDER    save to FOLDER instead of the folder in Settings
  --name FILE     save as FILE (one link only)
  --silent        add without showing the panel
  --queue-only    add to the queue without starting, without the panel
  --new-instance  start even if Syphr is already running

A running Syphr gets the links instead of a second window opening.";

/// What the command line asks for; `new_instance` skips handing the request to a running Syphr.
#[derive(Debug, PartialEq)]
pub struct Launch {
    pub request: AddRequest,
    pub new_instance: bool,
}

/// Reads the arguments after the program name. `Err` holds what to print: the usage, or a problem with it.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let mut launch = Launch { request: AddRequest::default(), new_instance: false };
    let request = &mut launch.request;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline.clone().or_else(|| args.next()).filter(|v| !v.is_empty()).ok_or(format!("{name} needs a value."))
        };
        match flag.as_str() {
            "--dir" => {
                let dir = value("--dir")?;
                // The running app has its own working folder, so a relative path is resolved here.
                let dir = std::path::absolute(&dir).map_err(|e| format!("--dir: {e}"))?;
                request.dir = Some(dir.display().to_string());
            }
            "--name" => request.name = Some(value("--name")?),
            "--silent" => request.silent = true,
            "--queue-only" => request.queue_only = true,
            "--new-instance" => launch.new_instance = true,
            "-h" | "--help" => return Err(USAGE.into()),
            // Older macOS adds a process serial number when launched from Finder.
            psn if psn.starts_with("-psn_") => {}
            link if crate::links::is_web_link(link) => request.urls.push(link.into()),
            link if let Some(scheme) = from_scheme(link) => request.urls.extend(scheme.urls),
            other => return Err(format!("Not a link or option: {other}\n\n{USAGE}")),
        }
    }
    if request.name.is_some() && request.urls.len() > 1 {
        return Err("--name works with one link only.".into());
    }
    Ok(launch)
}

/// A `syphr://add?url=LINK` link, as a browser or another app opens it. Only the link is taken: such links can come
/// from any web page, so the panel always shows and the folder is the user's.
pub fn from_scheme(link: &str) -> Option<AddRequest> {
    let parsed = reqwest::Url::parse(link).ok()?;
    if parsed.scheme() != "syphr" || parsed.host_str() != Some("add") {
        return None;
    }
    let url = parsed.query_pairs().find(|(key, _)| key == "url")?.1.into_owned();
    crate::links::is_web_link(&url).then(|| AddRequest { urls: vec![url], ..Default::default() })
}

/// Where the running Syphr writes its port and secret, readable only by this user.
fn instance_file() -> PathBuf {
    crate::ui::settings::data_dir().join("instance")
}

/// Hands `request` to a Syphr that is already running; false when none answers, such as after a crash left the file.
pub fn forward(request: &AddRequest) -> bool {
    let Ok(text) = fs::read_to_string(instance_file()) else { return false };
    let Some((port, secret)) = text.trim().split_once(' ') else { return false };
    let Ok(port) = port.parse::<u16>() else { return false };
    let Ok(stream) = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), TIMEOUT) else { return false };
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let message = format!("{secret}\n{}\n", serde_json::to_string(request).expect("request serializes"));
    let mut reply = String::new();
    (&stream).write_all(message.as_bytes()).is_ok()
        && BufReader::new(&stream).read_line(&mut reply).is_ok()
        && reply.trim() == "ok"
}

/// Takes requests from later launches until the app quits and sends them to `requests`. Listens on loopback; each
/// request must carry the secret from the instance file, so other users on the machine can't add downloads.
pub fn listen(requests: async_channel::Sender<AddRequest>) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => listener,
        Err(e) => return eprintln!("Couldn't listen for other launches: {e}"),
    };
    let secret = new_secret();
    let written =
        listener.local_addr().and_then(|addr| write_private(&instance_file(), &format!("{} {secret}", addr.port())));
    if let Err(e) = written {
        return eprintln!("Couldn't write {}: {e}", instance_file().display());
    }
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Some(request) = receive(&stream, &secret) {
                let delivered = requests.send_blocking(request).is_ok();
                (&stream).write_all(if delivered { b"ok\n" } else { b"no\n" }).ok();
            }
        }
    });
}

const TIMEOUT: Duration = Duration::from_secs(2);

fn receive(stream: &TcpStream, secret: &str) -> Option<AddRequest> {
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    let mut reader = BufReader::new(stream.take(1 << 20));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.trim_end() != secret {
        return None;
    }
    line.clear();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn new_secret() -> String {
    // RandomState is keyed from the OS's random source, so its hashes can't be predicted by another process.
    let state = RandomState::new();
    let mut sha = Sha256::new();
    for i in 0..4u32 {
        sha.update(state.hash_one((i, SystemTime::now(), std::process::id())).to_le_bytes());
    }
    format!("{:x}", sha.finalize())
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("instance file has a parent"))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn parses_command_lines() {
        // An absolute path for this OS; "/tmp/d" gains a drive letter on Windows.
        let dir = std::env::temp_dir().join("d").display().to_string();
        let launch = parse(&["https://a.com/x.zip", "--dir", &dir, "--name=y.zip", "--silent"]).unwrap();
        assert_eq!(
            launch,
            Launch {
                request: AddRequest {
                    urls: vec!["https://a.com/x.zip".into()],
                    dir: Some(dir),
                    name: Some("y.zip".into()),
                    silent: true,
                    queue_only: false,
                },
                new_instance: false,
            }
        );
        let launch =
            parse(&["--queue-only", "https://a.com/1", "syphr://add?url=https%3A%2F%2Fb.com%2F2%3Fq%3D1"]).unwrap();
        assert_eq!(launch.request.urls, ["https://a.com/1", "https://b.com/2?q=1"]);
        assert!(launch.request.queue_only);
        assert!(parse(&["--new-instance", "-psn_0_123"]).unwrap().new_instance);
        let rel = parse(&["--dir", "rel"]).unwrap().request.dir.unwrap();
        assert!(Path::new(&rel).is_absolute() && Path::new(&rel).ends_with("rel"));
        assert_eq!(parse(&[]).unwrap().request, AddRequest::default());
        assert!(parse(&["--dir"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["notalink"]).is_err());
        assert!(parse(&["--name", "n", "https://a.com/1", "https://a.com/2"]).is_err());
        assert!(parse(&["--help"]).unwrap_err().starts_with("Usage"));
        assert_eq!(from_scheme("syphr://add?url=file:///x"), None);
        assert_eq!(from_scheme("https://add?url=https://x.com/y"), None);
    }

    #[test]
    fn forwards_to_the_listener_with_the_secret() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let request = AddRequest { urls: vec!["https://a.com/x".into()], ..Default::default() };
        let sent = request.clone();
        let client = std::thread::spawn(move || {
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            (&stream).write_all(format!("s3cret\n{}\n", serde_json::to_string(&sent).unwrap()).as_bytes()).unwrap();
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            (&stream).write_all(b"wrong\n{}\n").unwrap();
        });
        let mut incoming = listener.incoming();
        assert_eq!(receive(&incoming.next().unwrap().unwrap(), "s3cret"), Some(request));
        assert_eq!(receive(&incoming.next().unwrap().unwrap(), "s3cret"), None);
        client.join().unwrap();
        assert_ne!(new_secret(), new_secret());
    }
}
