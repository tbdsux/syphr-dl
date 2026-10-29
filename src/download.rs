use std::path::PathBuf;

use gpui_kit::SharedString;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Downloading,
    Paused,
    Queued,
    Completed,
    /// Completed, but the file is no longer where it was saved.
    Missing,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Video,
    Music,
    Document,
    Program,
    Archive,
}

/// Where the bytes come from: a plain HTTP(S) file, or a page resolved through yt-dlp.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Direct,
    YtDlp,
}

#[derive(Clone, Debug)]
pub struct Download {
    pub id: u64,
    pub name: SharedString,
    pub url: SharedString,
    pub kind: Kind,
    pub source: Source,
    pub status: Status,
    pub total_bytes: Option<u64>,
    pub received_bytes: u64,
    pub bytes_per_sec: u64,
    pub folder: SharedString,
    /// The yt-dlp choice, e.g. "1080p60"; None for direct links.
    pub format: Option<SharedString>,
    /// The yt-dlp `-f` selector behind `format`.
    pub selector: Option<String>,
    /// The name the user typed; None uses the server's name or the video title.
    pub file_name: Option<String>,
    /// Where the finished file was saved.
    pub path: Option<PathBuf>,
    /// Each parallel part's finished fraction (0–1); empty for single-connection downloads.
    pub fragments: Vec<f32>,
}

impl Download {
    pub fn progress(&self) -> f32 {
        match self.total_bytes {
            Some(total) if total > 0 => (self.received_bytes as f32 / total as f32 * 100.).min(100.),
            _ => 0.,
        }
    }

    pub fn eta_secs(&self) -> Option<u64> {
        let total = self.total_bytes?;
        (self.status == Status::Downloading && self.bytes_per_sec > 0)
            .then(|| total.saturating_sub(self.received_bytes) / self.bytes_per_sec)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

pub fn format_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_progress() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_duration(59), "59s");
        assert_eq!(format_duration(125), "2m 05s");
        assert_eq!(format_duration(7260), "2h 01m");

        const MB: u64 = 1024 * 1024;
        let d = &Download {
            id: 1,
            name: "a.mp4".into(),
            url: "https://a.com/a.mp4".into(),
            kind: Kind::Video,
            source: Source::Direct,
            status: Status::Downloading,
            total_bytes: Some(48 * MB),
            received_bytes: 31 * MB,
            bytes_per_sec: 4 * MB,
            folder: "~/Downloads".into(),
            format: None,
            selector: None,
            file_name: None,
            path: None,
            fragments: Vec::new(),
        };
        assert!((d.progress() - 64.58).abs() < 0.1);
        assert_eq!(d.eta_secs(), Some(4));
        let unknown = Download { total_bytes: None, ..d.clone() };
        assert_eq!(unknown.progress(), 0.);
        assert_eq!(unknown.eta_secs(), None);
    }
}
