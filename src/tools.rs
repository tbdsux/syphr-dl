//! Finds yt-dlp and ffmpeg, and installs Syphr's own copies into Application Support when they're missing.
//! A user's own install wins; Syphr's yt-dlp keeps itself current with `-U`, since sites break old versions.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

use crate::engine::{CLIENT, RUNTIME};
use crate::ui::settings::AppSettings;

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
const YTDLP_SUMS: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";

/// Pinned builds with their SHA-256, so a changed file on the mirror is rejected. Bump these together.
const FFMPEG_ARM64: [(&str, &str, &str); 2] = [
    (
        "ffmpeg",
        "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip",
        "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924",
    ),
    (
        "ffprobe",
        "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffprobe.zip",
        "fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6",
    ),
];
const FFMPEG_X86_64: [(&str, &str, &str); 2] = [
    (
        "ffmpeg",
        "https://ffmpeg.martin-riedl.de/download/macos/amd64/1789931006_9.0.2/ffmpeg.zip",
        "7c6b4125b191cbf773832dc51f424cf2b6bb7da43007d1e066f95909e47cacd4",
    ),
    (
        "ffprobe",
        "https://ffmpeg.martin-riedl.de/download/macos/amd64/1789931006_9.0.2/ffprobe.zip",
        "2322438ed2f6319a691291b247d09c69dcaa3a982460d1f269a7e1af335cfdfd",
    ),
];

pub fn bin_dir() -> PathBuf {
    crate::ui::settings::home().join("Library/Application Support/Syphr/bin")
}

/// A full path is used as is. A bare name is searched on PATH, then in Homebrew's folders
/// (apps opened from Finder don't get the shell's PATH), then in Syphr's own folder.
pub fn find(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        return Some(PathBuf::from(bin)).filter(|p| p.is_file());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .chain([bin_dir()])
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

/// What to run: the found path, or the name as typed so errors mention it.
pub fn resolve(bin: &str) -> String {
    find(bin).map(|p| p.display().to_string()).unwrap_or_else(|| bin.to_string())
}

pub fn missing(cx: &App) -> bool {
    let s = AppSettings::get(cx);
    find(&s.ytdlp_path).is_none() || find(&s.ffmpeg_path).is_none()
}

/// Set while an install runs: what it's doing now.
#[derive(Default)]
pub struct Installer(pub Option<SharedString>);

impl Global for Installer {}

pub fn installing(cx: &App) -> Option<SharedString> {
    cx.try_global::<Installer>().and_then(|i| i.0.clone())
}

/// "Install required tools", showing the stage while an install runs; None when nothing is missing.
pub fn install_button(cx: &App) -> Option<Button> {
    let stage = installing(cx);
    (stage.is_some() || missing(cx)).then(|| {
        Button::new("install-tools")
            .primary()
            .label(stage.clone().unwrap_or("Install required tools".into()))
            .loading(stage.is_some())
            .on_click(|_, window, cx| install(window.window_handle(), cx))
    })
}

fn set_stage(stage: Option<&str>, cx: &mut App) {
    cx.set_global(Installer(stage.map(|s| SharedString::from(s.to_string()))));
    cx.refresh_windows();
}

/// Installs whichever of yt-dlp and ffmpeg can't be found, then points the settings at them.
/// Reports the outcome as a notification in `window`.
pub fn install(window: AnyWindowHandle, cx: &mut App) {
    if installing(cx).is_some() {
        return;
    }
    let s = AppSettings::get(cx);
    let (need_ytdlp, need_ffmpeg) = (find(&s.ytdlp_path).is_none(), find(&s.ffmpeg_path).is_none());
    set_stage(Some("Starting…"), cx);
    cx.spawn(async move |cx| {
        let dir = bin_dir();
        let result: Result<(), String> = async {
            fs::create_dir_all(&dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
            if need_ytdlp {
                cx.update(|cx| set_stage(Some("Downloading yt-dlp…"), cx));
                let ytdlp = RUNTIME.spawn(install_ytdlp(dir.clone())).await.map_err(|e| e.to_string())??;
                cx.update(|cx| AppSettings::update(cx, |s| s.ytdlp_path = ytdlp.display().to_string()));
            }
            if need_ffmpeg {
                cx.update(|cx| set_stage(Some("Downloading ffmpeg…"), cx));
                let ffmpeg = RUNTIME.spawn(install_ffmpeg(dir.clone())).await.map_err(|e| e.to_string())??;
                cx.update(|cx| AppSettings::update(cx, |s| s.ffmpeg_path = ffmpeg.display().to_string()));
            }
            Ok(())
        }
        .await;
        let message = match result {
            Ok(()) => "yt-dlp and ffmpeg are installed and ready.".to_string(),
            Err(e) => format!("Couldn't install the required tools: {e}"),
        };
        cx.update(|cx| {
            set_stage(None, cx);
            cx.update_window(window, |_, window, cx| window.push_notification(message, cx)).ok();
        });
    })
    .detach();
}

/// Lets Syphr's own yt-dlp update itself in the background; a Homebrew copy is left to Homebrew.
pub fn update_ytdlp(cx: &mut App) {
    let bin = PathBuf::from(&AppSettings::get(cx).ytdlp_path);
    if !bin.starts_with(bin_dir()) || !bin.is_file() {
        return;
    }
    cx.background_spawn(async move {
        if let Err(e) = Command::new(&bin).arg("-U").output() {
            eprintln!("yt-dlp update failed: {e}");
        }
    })
    .detach();
}

async fn install_ytdlp(dir: PathBuf) -> Result<PathBuf, String> {
    let sums = String::from_utf8_lossy(&fetch(YTDLP_SUMS).await?).into_owned();
    let expected = sums
        .lines()
        .find_map(|l| l.strip_suffix("  yt-dlp_macos"))
        .ok_or("yt-dlp's checksum list has no macOS entry")?
        .to_string();
    let tmp = dir.join("yt-dlp.download");
    fs::write(&tmp, fetch(YTDLP_URL).await?).map_err(|e| e.to_string())?;
    verify(&tmp, &expected)?;
    let path = dir.join("yt-dlp");
    make_executable(&tmp)?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

async fn install_ffmpeg(dir: PathBuf) -> Result<PathBuf, String> {
    let builds = match std::env::consts::ARCH {
        "aarch64" => FFMPEG_ARM64,
        "x86_64" => FFMPEG_X86_64,
        arch => return Err(format!("no ffmpeg build for {arch}")),
    };
    for (name, url, sha) in builds {
        let zip = dir.join(format!("{name}.zip"));
        fs::write(&zip, fetch(url).await?).map_err(|e| e.to_string())?;
        verify(&zip, sha)?;
        // `ditto` ships with macOS and unzips without adding a dependency.
        let ok =
            Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip).arg(&dir).status().is_ok_and(|s| s.success());
        fs::remove_file(&zip).ok();
        if !ok {
            return Err(format!("couldn't unpack {name}"));
        }
        make_executable(&dir.join(name))?;
    }
    Ok(dir.join("ffmpeg"))
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let resp = CLIENT.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// Deletes the file when its SHA-256 doesn't match, so a bad download is never run.
fn verify(path: &Path, expected: &str) -> Result<(), String> {
    let out = Command::new("/usr/bin/shasum").args(["-a", "256"]).arg(path).output().map_err(|e| e.to_string())?;
    let actual = String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_string();
    if out.status.success() && actual.eq_ignore_ascii_case(expected.trim()) {
        return Ok(());
    }
    fs::remove_file(path).ok();
    Err(format!("{} failed its checksum check", path.file_name().unwrap_or_default().to_string_lossy()))
}

fn make_executable(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{find, resolve, verify};

    #[test]
    fn finds_full_paths_and_names() {
        assert_eq!(find("/bin/sh"), Some(PathBuf::from("/bin/sh")));
        assert_eq!(find("/no/such/tool"), None);
        assert!(find("sh").is_some());
        assert_eq!(find("no-such-tool-syphr"), None);
        assert_eq!(resolve("no-such-tool-syphr"), "no-such-tool-syphr");
    }

    #[test]
    fn rejects_wrong_checksum() {
        let dir = std::env::temp_dir().join(format!("syphr-sum-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x");
        fs::write(&file, b"hello").unwrap();
        verify(&file, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824").unwrap();
        assert!(verify(&file, &"0".repeat(64)).is_err());
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod live {
    use std::{process::Command, time::Duration};

    use super::{install_ffmpeg, install_ytdlp};
    use crate::engine::{Job, RUNTIME, Request};

    #[test]
    #[ignore = "network"]
    fn installs_and_merges() {
        let dir = std::env::temp_dir().join(format!("syphr-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ytdlp = RUNTIME.block_on(install_ytdlp(dir.clone())).unwrap();
        let ffmpeg = RUNTIME.block_on(install_ffmpeg(dir.clone())).unwrap();
        for (bin, flag) in [(&ytdlp, "--version"), (&ffmpeg, "-version"), (&dir.join("ffprobe"), "-version")] {
            assert!(Command::new(bin).arg(flag).status().unwrap().success(), "{}", bin.display());
        }
        let mut job = Job::default();
        job.start(Request {
            url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
            folder: dir.join("out"),
            selector: Some("bv*[height=240]+ba/b[height=240]".into()),
            file_name: None,
            ytdlp_bin: ytdlp.display().to_string(),
            ffmpeg_bin: ffmpeg.display().to_string(),
            limit_bps: 0,
            fragments: 4,
        });
        let path = loop {
            std::thread::sleep(Duration::from_millis(300));
            if let Some(result) = job.take_result() {
                break result.unwrap();
            }
        };
        eprintln!("merged: {} ({} bytes)", path.display(), std::fs::metadata(&path).unwrap().len());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
