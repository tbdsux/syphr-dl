//! Finds yt-dlp and ffmpeg, and installs Syphr's own copies into Application Support when they're missing.
//! A user's own install wins; Syphr's yt-dlp keeps itself current with `-U`, since sites break old versions.

use std::{
    env::consts::EXE_SUFFIX,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use sha2::{Digest, Sha256};

use crate::engine::{CLIENT, RUNTIME};
use crate::ui::settings::AppSettings;

const YTDLP_ASSET: &str = if cfg!(windows) {
    "yt-dlp.exe"
} else if cfg!(target_os = "macos") {
    "yt-dlp_macos"
} else if cfg!(target_arch = "aarch64") {
    "yt-dlp_linux_aarch64"
} else {
    "yt-dlp_linux"
};
const YTDLP_SUMS: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";

/// Pinned builds with their SHA-256, so a changed file on the mirror is rejected. Bump these together.
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
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
/// gyan.dev's essentials build (x64; Windows on Arm runs it emulated), with ffmpeg.exe and ffprobe.exe under `bin/`.
#[cfg(windows)]
const FFMPEG_WINDOWS: (&str, &str, &str) = (
    "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip",
    "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba",
    "ffmpeg-9.0.2-essentials_build/bin",
);

pub fn bin_dir() -> PathBuf {
    crate::ui::settings::data_dir().join("bin")
}

/// A full path is used as is. A bare name is searched on PATH, then in Homebrew's folders
/// (apps opened from Finder don't get the shell's PATH), then in Syphr's own folder.
/// On Windows a bare name also matches its `.exe`.
pub fn find(bin: &str) -> Option<PathBuf> {
    if bin.contains(std::path::is_separator) {
        return Some(PathBuf::from(bin)).filter(|p| p.is_file());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let homebrew: &[&str] = if cfg!(windows) { &[] } else { &["/opt/homebrew/bin", "/usr/local/bin"] };
    std::env::split_paths(&path)
        .chain(homebrew.iter().map(PathBuf::from))
        .chain([bin_dir()])
        .flat_map(|dir| [dir.join(bin), dir.join(format!("{bin}{EXE_SUFFIX}"))])
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
        .find_map(|l| l.strip_suffix(&format!("  {YTDLP_ASSET}")))
        .ok_or(format!("yt-dlp's checksum list has no {YTDLP_ASSET} entry"))?
        .to_string();
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{YTDLP_ASSET}");
    let bytes = fetch(&url).await?;
    verify(&bytes, &expected, "yt-dlp")?;
    let tmp = dir.join("yt-dlp.download");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    let path = dir.join(format!("yt-dlp{EXE_SUFFIX}"));
    make_executable(&tmp)?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(target_os = "macos")]
async fn install_ffmpeg(dir: PathBuf) -> Result<PathBuf, String> {
    let builds = match std::env::consts::ARCH {
        "aarch64" => FFMPEG_ARM64,
        "x86_64" => FFMPEG_X86_64,
        arch => return Err(format!("no ffmpeg build for {arch}")),
    };
    for (name, url, sha) in builds {
        let bytes = fetch(url).await?;
        verify(&bytes, sha, name)?;
        let zip = dir.join(format!("{name}.zip"));
        fs::write(&zip, bytes).map_err(|e| e.to_string())?;
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

// shortcut: no pinned Linux build, since unpacking one needs unzip or xz the system may lack; pin a static build if users ask.
#[cfg(not(any(windows, target_os = "macos")))]
async fn install_ffmpeg(_: PathBuf) -> Result<PathBuf, String> {
    Err("install ffmpeg with your package manager, e.g. `sudo apt install ffmpeg`".into())
}

#[cfg(windows)]
async fn install_ffmpeg(dir: PathBuf) -> Result<PathBuf, String> {
    let (url, sha, inner) = FFMPEG_WINDOWS;
    let bytes = fetch(url).await?;
    verify(&bytes, sha, "ffmpeg")?;
    let zip = dir.join("ffmpeg.zip");
    fs::write(&zip, bytes).map_err(|e| e.to_string())?;
    // Windows 10+ ships bsdtar, which reads zips; only the two programs are pulled out.
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let ok = Command::new(Path::new(&root).join("System32").join("tar.exe"))
        .args(["--strip-components", "2", "-xf"])
        .arg(&zip)
        .arg("-C")
        .arg(&dir)
        .args([format!("{inner}/ffmpeg.exe"), format!("{inner}/ffprobe.exe")])
        .status()
        .is_ok_and(|s| s.success());
    fs::remove_file(&zip).ok();
    if !ok {
        return Err("couldn't unpack ffmpeg".into());
    }
    Ok(dir.join("ffmpeg.exe"))
}

pub(crate) async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let resp = CLIENT.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// Checked before anything is written, so a bad download never reaches the disk.
pub(crate) fn verify(bytes: &[u8], expected: &str, name: &str) -> Result<(), String> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected.trim()) { Ok(()) } else { Err(format!("{name} failed its checksum check")) }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{find, resolve, verify};

    #[test]
    fn finds_full_paths_and_names() {
        #[cfg(unix)]
        {
            assert_eq!(find("/bin/sh"), Some(std::path::PathBuf::from("/bin/sh")));
            assert!(find("sh").is_some());
        }
        #[cfg(windows)]
        assert!(find("cmd").is_some_and(|p| p.ends_with("cmd.exe")));
        assert_eq!(find("/no/such/tool"), None);
        assert_eq!(find("no-such-tool-syphr"), None);
        assert_eq!(resolve("no-such-tool-syphr"), "no-such-tool-syphr");
    }

    #[test]
    fn rejects_wrong_checksum() {
        verify(b"hello", "2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824", "x").unwrap();
        assert!(verify(b"hello", &"0".repeat(64), "x").is_err());
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
        for (bin, flag) in [
            (&ytdlp, "--version"),
            (&ffmpeg, "-version"),
            (&dir.join(format!("ffprobe{}", std::env::consts::EXE_SUFFIX)), "-version"),
        ] {
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
