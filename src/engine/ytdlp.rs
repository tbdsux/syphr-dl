//! Looks up a page with `yt-dlp -J` and turns its format list into a few choices a person can pick,
//! then runs the chosen download.

use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::Ordering::Relaxed,
};

use gpui_kit::SharedString;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::names::{file_name_of, sanitize, unique_path};
use super::{Request, Shared, err};

/// shortcut: a fixed host list decides when to ask yt-dlp; switch to `yt-dlp --simulate` probing if users need more sites.
const HOSTS: [&str; 7] =
    ["youtube.com", "youtu.be", "vimeo.com", "twitch.tv", "soundcloud.com", "dailymotion.com", "x.com"];

pub fn handles(url: &str) -> bool {
    let host = url.split('/').nth(2).unwrap_or_default().to_lowercase();
    HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub title: SharedString,
    pub uploader: Option<SharedString>,
    pub duration_secs: Option<u64>,
    pub options: Vec<FormatOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormatOption {
    pub label: SharedString,
    pub detail: SharedString,
    /// Passed to `yt-dlp -f`.
    pub selector: String,
    pub size_bytes: Option<u64>,
    pub audio_only: bool,
}

#[derive(Deserialize)]
struct RawInfo {
    title: String,
    uploader: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    formats: Vec<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: String,
    ext: String,
    height: Option<u32>,
    fps: Option<f64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    abr: Option<f64>,
}

impl RawFormat {
    fn size(&self) -> Option<u64> {
        self.filesize.or(self.filesize_approx)
    }
    fn has_video(&self) -> bool {
        self.vcodec.as_deref().is_some_and(|c| c != "none") && self.height.is_some()
    }
    fn has_audio(&self) -> bool {
        self.acodec.as_deref().is_some_and(|c| c != "none")
    }
}

/// Blocking: run it on a background executor.
pub fn probe(bin: &str, url: &str) -> Result<VideoInfo, String> {
    let output =
        Command::new(bin).args(["-J", "--no-playlist", "--no-warnings", url]).output().map_err(|err| {
            match err.kind() {
                ErrorKind::NotFound => {
                    format!("Couldn't find “{bin}”. Install the required tools to download from this site.")
                }
                _ => format!("Couldn't run {bin}: {err}"),
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(reason.trim_start_matches("ERROR: ").to_string());
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

pub fn parse(json: &str) -> Result<VideoInfo, String> {
    let raw: RawInfo =
        serde_json::from_str(json).map_err(|err| format!("yt-dlp returned data Syphr can't read: {err}"))?;

    let audio_only = || raw.formats.iter().filter(|f| f.has_audio() && !f.has_video());
    let best_audio = best_of(audio_only());

    // Best video stream per height, highest first.
    let mut by_height: BTreeMap<u32, &RawFormat> = BTreeMap::new();
    for f in raw.formats.iter().filter(|f| f.has_video()) {
        let height = f.height.unwrap_or_default();
        let better = by_height.get(&height).is_none_or(|cur| {
            (f.fps.unwrap_or(0.), f.size().unwrap_or(0)) > (cur.fps.unwrap_or(0.), cur.size().unwrap_or(0))
        });
        if better {
            by_height.insert(height, f);
        }
    }

    let mut options = Vec::new();
    for (height, f) in by_height.iter().rev() {
        // Audio in the same container family merges without falling back to mkv.
        let audio = if f.has_audio() {
            None
        } else {
            best_of(audio_only().filter(|a| merged_ext(&f.ext, &a.ext) == f.ext)).or(best_audio)
        };
        let audio_size = match audio {
            Some(a) => a.size(),
            None => Some(0),
        };
        let size = f.size().zip(audio_size).map(|(v, a)| v + a);
        let fps = f.fps.filter(|fps| *fps > 30.).map(|fps| format!("{fps:.0}")).unwrap_or_default();
        // Exact format ids, so the file matches what the label promised.
        let (selector, ext) = match audio {
            Some(a) => (format!("{}+{}", f.format_id, a.format_id), merged_ext(&f.ext, &a.ext)),
            None => (f.format_id.clone(), f.ext.as_str()),
        };
        options.push(FormatOption {
            label: format!("{height}p{fps}").into(),
            detail: describe(ext, size),
            selector: format!("{selector}/b[height={height}]"),
            size_bytes: size,
            audio_only: false,
        });
    }
    if let Some(best) = options.first() {
        let best = FormatOption { label: "Best quality".into(), ..best.clone() };
        options.insert(0, best);
    }
    if let Some(audio) = best_audio {
        let kbps = audio.abr.map(|abr| format!(" · {abr:.0} kbps")).unwrap_or_default();
        options.push(FormatOption {
            label: "Audio only".into(),
            detail: format!("{}{kbps}", describe(&audio.ext, audio.size())).into(),
            selector: format!("ba[ext={}]/ba", audio.ext),
            size_bytes: audio.size(),
            audio_only: true,
        });
    }
    if options.is_empty() {
        return Err("yt-dlp found no downloadable formats for this link.".into());
    }

    Ok(VideoInfo {
        title: raw.title.into(),
        uploader: raw.uploader.map(Into::into),
        duration_secs: raw.duration.map(|d| d as u64),
        options,
    })
}

fn best_of<'a>(audio: impl Iterator<Item = &'a RawFormat>) -> Option<&'a RawFormat> {
    audio.max_by(|a, b| a.abr.unwrap_or(0.).total_cmp(&b.abr.unwrap_or(0.)))
}

/// The container yt-dlp merges video and audio into by default.
fn merged_ext<'a>(video: &'a str, audio: &str) -> &'a str {
    match (video, audio) {
        ("mp4", "m4a" | "mp4") | ("webm", "webm") => video,
        _ => "mkv",
    }
}

fn describe(ext: &str, size: Option<u64>) -> SharedString {
    match size {
        Some(size) => format!("{} · ~{}", ext.to_uppercase(), crate::download::format_bytes(size)),
        None => format!("{} · size unknown", ext.to_uppercase()),
    }
    .into()
}

pub(super) async fn run_ytdlp(request: &Request, selector: &str, shared: &Shared) -> Result<PathBuf, String> {
    // Every choice except audio-only joins two streams, which yt-dlp can't do without ffmpeg.
    if selector.contains('+') {
        let ffmpeg = tokio::process::Command::new(&request.ffmpeg_bin)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if !ffmpeg.is_ok_and(|s| s.success()) {
            return Err(format!(
                "ffmpeg is required to join video and audio. Use Install required tools in Settings. Tried “{}”.",
                request.ffmpeg_bin
            ));
        }
    }
    // yt-dlp skips a file that already exists instead of renaming, so it saves into a staging folder
    // and the finished file is moved out under a free name. The stable folder lets a paused download resume.
    let staging = request.folder.join(".syphr-partial");
    let template = match request.file_name.as_deref().map(sanitize).filter(|n| !n.is_empty()) {
        Some(name) => format!("{}.%(ext)s", name.replace('%', "%%")),
        None => "%(title)s.%(ext)s".into(),
    };
    let mut cmd = tokio::process::Command::new(&request.ytdlp_bin);
    cmd.args([
        "-f",
        selector,
        "-N",
        &request.fragments.max(1).to_string(),
        "--no-playlist",
        "--newline",
        "--progress",
        "--no-warnings",
    ])
    .args([
        "--progress-template",
        "download:%(progress.downloaded_bytes)s %(progress.total_bytes,progress.total_bytes_estimate)s",
    ])
    .args(["--print", "after_move:filepath", "-o", &template, "-P"])
    .arg(&staging);
    // A bare command name is found on PATH by yt-dlp itself.
    if request.ffmpeg_bin.contains(std::path::is_separator) {
        cmd.arg("--ffmpeg-location").arg(&request.ffmpeg_bin);
    }
    if request.limit_bps > 0 {
        cmd.arg("-r").arg(request.limit_bps.to_string());
    }
    let mut child = cmd
        .arg("--")
        .arg(&request.url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Couldn't run {}: {e}", request.ytdlp_bin))?;

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
    let read_out = async {
        let (mut base, mut last, mut path) = (0u64, 0u64, None);
        while let Ok(Some(line)) = stdout.next_line().await {
            match parse_progress(&line) {
                Some((done, total)) => {
                    // Video and audio download one after another, each counting from zero.
                    if done < last {
                        base += last;
                    }
                    last = done;
                    shared.received.store(base + done, Relaxed);
                    if let Some(total) = total {
                        shared.total.fetch_max(base + total, Relaxed);
                    }
                }
                None if !line.trim().is_empty() => path = Some(PathBuf::from(line.trim())),
                None => {}
            }
        }
        path
    };
    let read_err = async {
        let mut last = None;
        while let Ok(Some(line)) = stderr.next_line().await {
            if !line.trim().is_empty() {
                last = Some(line.trim().trim_start_matches("ERROR: ").to_string());
            }
        }
        last
    };
    let (path, last_err) = tokio::join!(read_out, read_err);
    let status = child.wait().await.map_err(err)?;
    match path {
        Some(path) if status.success() => {
            let dest = unique_path(&request.folder, &file_name_of(&path));
            fs::rename(&path, &dest).map_err(err)?;
            // Fails while another download is still using it, which is fine.
            let _ = fs::remove_dir(&staging);
            Ok(dest)
        }
        _ => Err(last_err.unwrap_or_else(|| format!("yt-dlp exited with {status}"))),
    }
}

/// "1234 5678" from our progress template; yt-dlp writes "NA" for unknown values.
fn parse_progress(line: &str) -> Option<(u64, Option<u64>)> {
    let (done, total) = line.trim().split_once(' ')?;
    let done = done.parse::<f64>().ok()? as u64;
    Some((done, total.parse::<f64>().ok().map(|t| t as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "title": "Rust in 100 Seconds", "uploader": "Fireship", "duration": 151.0,
        "formats": [
            {"format_id": "sb0", "ext": "mhtml", "vcodec": "none", "acodec": "none"},
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 129.5, "filesize": 2400000},
            {"format_id": "251", "ext": "webm", "vcodec": "none", "acodec": "opus", "abr": 110.0, "filesize": 2100000},
            {"format_id": "18", "ext": "mp4", "height": 360, "fps": 30, "vcodec": "avc1", "acodec": "mp4a", "filesize": 9000000},
            {"format_id": "136", "ext": "mp4", "height": 720, "fps": 30, "vcodec": "avc1", "acodec": "none", "filesize_approx": 20000000},
            {"format_id": "299", "ext": "mp4", "height": 1080, "fps": 60, "vcodec": "avc1", "acodec": "none", "filesize": 50000000},
            {"format_id": "137", "ext": "mp4", "height": 1080, "fps": 30, "vcodec": "avc1", "acodec": "none", "filesize": 40000000},
            {"format_id": "248", "ext": "webm", "height": 1080, "fps": 30, "vcodec": "vp9", "acodec": "none", "filesize": 30000000}
        ]
    }"#;

    #[test]
    fn builds_choices_from_formats() {
        let info = parse(SAMPLE).unwrap();
        assert_eq!(info.title, "Rust in 100 Seconds");
        assert_eq!(info.duration_secs, Some(151));
        let labels: Vec<_> = info.options.iter().map(|o| o.label.to_string()).collect();
        assert_eq!(labels, ["Best quality", "1080p60", "720p", "360p", "Audio only"]);
        // Video-only streams add the best audio track; muxed ones don't.
        assert_eq!(info.options[1].size_bytes, Some(52_400_000));
        assert_eq!(info.options[3].size_bytes, Some(9_000_000));
        assert_eq!(info.options[4].selector, "ba[ext=m4a]/ba");
        // Pinned ids with mp4-compatible audio, so the saved file is the MP4 the label shows.
        assert_eq!(info.options[1].selector, "299+140/b[height=1080]");
        assert!(info.options[1].detail.starts_with("MP4"));
        assert_eq!(info.options[3].selector, "18/b[height=360]");
        assert_eq!(merged_ext("webm", "m4a"), "mkv");
        assert!(info.options[4].audio_only);
    }

    #[test]
    fn rejects_bad_output() {
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"title": "x", "formats": []}"#).is_err());
    }

    #[test]
    fn matches_hosts() {
        assert!(handles("https://www.youtube.com/watch?v=1"));
        assert!(handles("https://youtu.be/1"));
        assert!(!handles("https://notyoutube.com/a.zip"));
        assert!(!handles("https://example.com/youtube.com.zip"));
    }

    #[test]
    fn progress_lines() {
        assert_eq!(parse_progress("1024 4096"), Some((1024, Some(4096))));
        assert_eq!(parse_progress("1024 NA"), Some((1024, None)));
        assert_eq!(parse_progress("/Users/me/Downloads/Video.mp4"), None);
    }
}
