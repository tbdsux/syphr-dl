//! Finds newer Syphr releases on GitHub and installs them in place, then restarts.
//! Only releases that ship a `SHA256SUMS` are offered, and the download is checked against it before anything is replaced.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use gpui_kit::component::WindowExt;
use gpui_kit::*;
use semver::Version;
use serde::Deserialize;

use crate::engine::RUNTIME;
use crate::tools::{fetch, verify};
use crate::ui::settings::AppSettings;

const RELEASES: &str = "https://api.github.com/repos/tbdsux/syphr-dl/releases?per_page=20";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The release file for this install, after `Syphr-<tag>`. Matches `release.yml`.
fn asset_suffix() -> &'static str {
    if cfg!(target_os = "macos") {
        ".dmg"
    } else if cfg!(windows) {
        "-windows-x64.zip"
    } else if appimage().is_some() {
        "-linux-x86_64.AppImage"
    } else {
        "-linux-x64.tar.gz"
    }
}

/// The AppImage file this copy runs from; its runtime sets `APPIMAGE`.
fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").filter(|_| cfg!(target_os = "linux")).map(PathBuf::from)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    file: String,
    url: String,
    sums: String,
}

#[derive(Default)]
pub struct Updater {
    pub available: Option<Release>,
    /// Set while a check or install runs: what it's doing now.
    pub busy: Option<SharedString>,
}

impl Global for Updater {}

pub fn available(cx: &App) -> Option<&Release> {
    cx.global::<Updater>().available.as_ref()
}

pub fn busy(cx: &App) -> Option<SharedString> {
    cx.global::<Updater>().busy.clone()
}

fn set_busy(busy: Option<SharedString>, cx: &mut App) {
    cx.global_mut::<Updater>().busy = busy;
    cx.refresh_windows();
}

fn notify(window: AnyWindowHandle, message: String, cx: &mut App) {
    cx.update_window(window, |_, window, cx| window.push_notification(message, cx)).ok();
}

/// Clears what the last update left behind, then checks now and every few hours while the setting is on.
pub fn start(cx: &mut App) {
    cx.set_global(Updater::default());
    if let Ok(exe) = std::env::current_exe() {
        // Windows can't delete a running exe, so the replaced one waits until the next launch.
        fs::remove_file(with_suffix(&exe, ".old")).ok();
    }
    fs::remove_dir_all(staging_dir()).ok();
    cx.spawn(async move |cx| {
        loop {
            if cx.update(|cx| AppSettings::get(cx).check_updates) {
                cx.update(|cx| check(None, cx));
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

/// Looks for a newer release. With a window, the outcome is reported there; without, it only shows in the UI.
pub fn check(window: Option<AnyWindowHandle>, cx: &mut App) {
    if busy(cx).is_some() {
        return;
    }
    set_busy(Some("Checking…".into()), cx);
    cx.spawn(async move |cx| {
        let result = RUNTIME.spawn(latest()).await.map_err(|e| e.to_string()).and_then(|r| r);
        cx.update(|cx| {
            set_busy(None, cx);
            let message = match &result {
                Ok(Some(r)) => format!("Syphr {} is available.", r.tag),
                Ok(None) => format!("Syphr {} is the latest version.", env!("CARGO_PKG_VERSION")),
                Err(e) => format!("Couldn't check for updates: {e}"),
            };
            if let Ok(release) = result {
                cx.global_mut::<Updater>().available = release;
            }
            if let Some(window) = window {
                notify(window, message, cx);
            }
        });
    })
    .detach();
}

/// Downloads the available release, swaps it in, and restarts into it.
pub fn install(window: AnyWindowHandle, cx: &mut App) {
    let Some(release) = available(cx).cloned() else { return };
    if busy(cx).is_some() {
        return;
    }
    set_busy(Some(format!("Updating to {}…", release.tag).into()), cx);
    cx.spawn(async move |cx| {
        let result = RUNTIME.spawn(download_and_replace(release)).await.map_err(|e| e.to_string()).and_then(|r| r);
        cx.update(|cx| match result.and_then(|app| relaunch(&app)) {
            Ok(()) => cx.quit(),
            Err(e) => {
                set_busy(None, cx);
                notify(window, format!("Couldn't update Syphr: {e}"), cx);
            }
        });
    })
    .detach();
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

async fn latest() -> Result<Option<Release>, String> {
    let releases: Vec<GhRelease> = serde_json::from_slice(&fetch(RELEASES).await?).map_err(|e| e.to_string())?;
    Ok(newest(releases, env!("CARGO_PKG_VERSION")))
}

/// The highest version above `current` with a file for this platform. Betas are offered only to beta builds.
fn newest(releases: Vec<GhRelease>, current: &str) -> Option<Release> {
    let current = Version::parse(current).ok()?;
    releases
        .into_iter()
        .filter(|r| !r.draft && (!r.prerelease || !current.pre.is_empty()))
        .filter_map(|r| {
            let version = Version::parse(r.tag_name.strip_prefix('v')?).ok().filter(|v| *v > current)?;
            let file = format!("Syphr-{}{}", r.tag_name, asset_suffix());
            let url = |name: &str| r.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
            Some((version, Release { url: url(&file)?, sums: url("SHA256SUMS")?, tag: r.tag_name.clone(), file }))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, release)| release)
}

fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("syphr-update")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

/// Returns what to launch: the app bundle on macOS, the executable elsewhere.
async fn download_and_replace(release: Release) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        return Err("development builds don't update themselves".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(target_os = "linux") && appimage().is_none() && exe.starts_with("/usr") {
        return Err("this copy was installed from the .deb, so install the new .deb from the release page".into());
    }
    let sums = String::from_utf8_lossy(&fetch(&release.sums).await?).into_owned();
    let expected = sums
        .lines()
        .find_map(|l| l.strip_suffix(&format!("  {}", release.file)))
        .ok_or(format!("the release's checksum list has no {} entry", release.file))?
        .to_string();
    let bytes = fetch(&release.url).await?;
    verify(&bytes, &expected, "the download")?;
    let dir = staging_dir();
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let file = dir.join(&release.file);
    fs::write(&file, bytes).map_err(|e| e.to_string())?;
    replace(&exe, &dir, &file)
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("couldn't {what}")),
    }
}

/// Mounts the DMG and copies its Syphr.app over the running one.
#[cfg(target_os = "macos")]
fn replace(exe: &Path, dir: &Path, dmg: &Path) -> Result<PathBuf, String> {
    // …/Syphr.app/Contents/MacOS/syphr-dl
    let app = exe
        .ancestors()
        .nth(3)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("only a packaged Syphr.app can update itself")?
        .to_path_buf();
    let mnt = dir.join("mnt");
    run(
        Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
            .arg(&mnt)
            .arg(dmg),
        "open the update",
    )?;
    let staged = with_suffix(&app, ".new");
    fs::remove_dir_all(&staged).ok();
    let copied = run(Command::new("/usr/bin/ditto").arg(mnt.join("Syphr.app")).arg(&staged), "copy the update");
    Command::new("/usr/bin/hdiutil").args(["detach", "-quiet"]).arg(&mnt).status().ok();
    copied?;
    swap(&app, &staged)?;
    Ok(app)
}

/// Unpacks the archive with the OS's own tar and puts its executable over the running one.
#[cfg(not(target_os = "macos"))]
fn replace(exe: &Path, dir: &Path, archive: &Path) -> Result<PathBuf, String> {
    // The download is the whole new AppImage, so it goes over the file, not the exe mounted from it.
    #[cfg(target_os = "linux")]
    if let Some(image) = appimage() {
        use std::os::unix::fs::PermissionsExt;
        let staged = with_suffix(&image, ".new");
        fs::copy(archive, &staged).map_err(|e| format!("can't write next to {}: {e}", image.display()))?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        swap(&image, &staged)?;
        return Ok(image);
    }
    // Windows 10+ ships bsdtar, which reads zips.
    let tar = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Path::new(&root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    run(Command::new(tar).arg("-xf").arg(archive).arg("-C").arg(dir), "unpack the update")?;
    // Staged beside the exe, so the swap is a rename on the same disk.
    let staged = with_suffix(exe, ".new");
    fs::copy(dir.join(format!("syphr-dl{}", std::env::consts::EXE_SUFFIX)), &staged)
        .map_err(|e| format!("can't write next to {}: {e}", exe.display()))?;
    swap(exe, &staged)?;
    Ok(exe.to_path_buf())
}

/// Moves `target` aside and `staged` into its place, putting `target` back if that fails.
fn swap(target: &Path, staged: &Path) -> Result<(), String> {
    let old = with_suffix(target, ".old");
    let remove = |p: &Path| fs::remove_dir_all(p).or_else(|_| fs::remove_file(p)).ok();
    remove(&old);
    fs::rename(target, &old).map_err(|e| format!("can't replace {}: {e}", target.display()))?;
    if let Err(e) = fs::rename(staged, target) {
        fs::rename(&old, target).ok();
        return Err(format!("can't replace {}: {e}", target.display()));
    }
    remove(&old);
    Ok(())
}

fn relaunch(app: &Path) -> Result<(), String> {
    // `-n`, or `open` would just bring this still-running copy forward.
    let mut cmd = if cfg!(target_os = "macos") {
        let mut open = Command::new("/usr/bin/open");
        open.arg("-n").arg(app);
        open
    } else {
        Command::new(app)
    };
    cmd.spawn().map(drop).map_err(|e| format!("updated, but couldn't restart: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{GhAsset, GhRelease, asset_suffix, newest};

    fn release(tag: &str, prerelease: bool, sums: bool) -> GhRelease {
        let asset = |name: String| GhAsset { browser_download_url: format!("https://x/{name}"), name };
        let mut assets = vec![asset(format!("Syphr-{tag}{}", asset_suffix()))];
        if sums {
            assets.push(asset("SHA256SUMS".into()));
        }
        GhRelease { tag_name: tag.into(), draft: false, prerelease, assets }
    }

    #[test]
    fn picks_highest_eligible_release() {
        let all = || {
            vec![
                release("v0.4.0", false, false),
                release("v0.3.1", false, true),
                release("v0.3.2-beta.1", true, true),
                release("v0.2.0", false, true),
            ]
        };
        // No SHA256SUMS on 0.4.0, so it's skipped; betas only reach beta builds.
        assert_eq!(newest(all(), "0.3.0").unwrap().tag, "v0.3.1");
        assert_eq!(newest(all(), "0.3.0-beta.1").unwrap().tag, "v0.3.2-beta.1");
        assert_eq!(newest(all(), "0.3.1"), None);
        let picked = newest(all(), "0.3.0").unwrap();
        assert_eq!(picked.file, format!("Syphr-v0.3.1{}", asset_suffix()));
        assert_eq!(picked.sums, "https://x/SHA256SUMS");
    }

    #[test]
    #[ignore = "network"]
    fn reads_github_releases() {
        crate::engine::RUNTIME.block_on(super::latest()).unwrap();
    }
}
