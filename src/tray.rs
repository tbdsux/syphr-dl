//! The menu bar icon on macOS and the notification-area icon on Windows and Linux.

use gpui_kit::{AnyWindowHandle, App, Entity, SystemNotification};
use image::{Rgba, imageops::FilterType};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::download::{Download, Status, format_bytes};
use crate::ui::{
    app::{SyphrApp, expand_home},
    settings::{self, AppSettings},
};

const NO_DOWNLOADS: &str = "No active downloads";
const MAX_ROWS: usize = 8;
const MAX_NAME_CHARS: usize = 32;

/// Adds the icon and its menu; it lives until the app quits.
pub fn install(window: AnyWindowHandle, app: Entity<SyphrApp>, cx: &mut App) {
    let (mut tray, menu) = match build() {
        Ok((tray, menu)) => (Some(tray), menu),
        Err(e) => return eprintln!("tray icon: {e}"),
    };

    // The list notifies on every change, including each progress tick.
    let (mut rows, mut shown) = (Vec::new(), Vec::new());
    let mut sync = move |app: Entity<SyphrApp>, cx: &mut App| {
        let labels = row_labels(app.read(cx).downloads());
        if labels != shown {
            sync_rows(&menu, &mut rows, &labels);
            shown = labels;
        }
    };
    sync(app.clone(), cx);
    cx.observe(&app, sync).detach();

    // The handlers run on the platform's thread (a worker on Linux), so hop onto the app's.
    let (tx, rx) = async_channel::unbounded();
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        menu_tx.try_send(e.id.0).ok();
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e
            && !cfg!(target_os = "macos")
        {
            tx.try_send("show".to_string()).ok();
        }
    }));
    cx.spawn(async move |cx| {
        while let Ok(id) = rx.recv().await {
            cx.update(|cx| match id.as_str() {
                "new" => {
                    crate::show_main_window(cx);
                    window.update(cx, |_, window, cx| app.update(cx, |this, cx| this.new_download(window, cx))).ok();
                }
                "pause" => app.update(cx, |this, cx| this.pause_all(cx)),
                "resume" => app.update(cx, |this, cx| this.resume_all(cx)),
                "folder" => open_downloads_folder(cx),
                "settings" => {
                    // The app may be in the background, where opening a window wouldn't bring it forward.
                    cx.activate(true);
                    settings::open_settings_window(cx);
                }
                "quit" => cx.quit(),
                // Show Syphr, a click on the icon, and the download rows.
                _ => crate::show_main_window(cx),
            });
        }
    })
    .detach();
    // Dropping it removes the icon; Windows would otherwise leave a ghost until hovered.
    cx.on_app_quit(move |_| {
        tray.take();
        async {}
    })
    .detach();
}

fn open_downloads_folder(cx: &mut App) {
    let dir = expand_home(&AppSettings::get(cx).download_dir);
    // Nothing may have been saved there yet, and opening a missing folder fails without a word.
    match std::fs::create_dir_all(&dir) {
        Ok(()) => cx.open_with_system(&dir),
        Err(e) => cx.show_system_notification(SystemNotification {
            tag: "open-downloads-folder".into(),
            title: "Couldn't open the downloads folder".into(),
            body: e.to_string().into(),
            actions: Vec::new(),
        }),
    }
}

/// One line per ongoing download, running ones first, or a single line saying there are none.
fn row_labels(downloads: &[Download]) -> Vec<String> {
    let mut active: Vec<&Download> = downloads
        .iter()
        .filter(|d| matches!(d.status, Status::Downloading | Status::Paused | Status::Queued))
        .collect();
    if active.is_empty() {
        return vec![NO_DOWNLOADS.to_string()];
    }
    active.sort_by_key(|d| d.status);
    let mut labels: Vec<String> = active.iter().take(MAX_ROWS).map(|d| row_label(d)).collect();
    if active.len() > MAX_ROWS {
        labels.push(format!("and {} more…", active.len() - MAX_ROWS));
    }
    labels
}

fn row_label(d: &Download) -> String {
    let mut name: String = d.name.chars().take(MAX_NAME_CHARS).collect();
    if d.name.chars().count() > MAX_NAME_CHARS {
        name.push('…');
    }
    // A lone & in a menu label marks a mnemonic, which would eat it from file names.
    let name = name.replace('&', "&&");
    let progress =
        if d.total_bytes.is_some() { format!("{:.0}%", d.progress()) } else { format_bytes(d.received_bytes) };
    match d.status {
        Status::Queued => format!("{name} — Queued"),
        Status::Paused => format!("{name} — Paused · {progress}"),
        _ if d.bytes_per_sec > 0 => format!("{name} — {progress} · {}/s", format_bytes(d.bytes_per_sec)),
        _ => format!("{name} — {progress}"),
    }
}

/// The rows sit at the top of the menu; clicking one brings the window back.
fn sync_rows(menu: &Menu, rows: &mut Vec<MenuItem>, labels: &[String]) {
    for (i, label) in labels.iter().enumerate() {
        match rows.get(i) {
            Some(row) => row.set_text(label),
            None => {
                let row = MenuItem::new(label, true, None);
                menu.insert(&row, i).ok();
                rows.push(row);
            }
        }
    }
    for row in rows.drain(labels.len()..) {
        menu.remove(&row).ok();
    }
    rows[0].set_enabled(labels[0] != NO_DOWNLOADS);
}

fn build() -> Result<(TrayIcon, Menu), Box<dyn std::error::Error>> {
    let item = |id: &str, label: &str| MenuItem::with_id(id, label, true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &PredefinedMenuItem::separator(),
        &item("show", "Show Syphr"),
        &item("new", "New Download…"),
        &PredefinedMenuItem::separator(),
        &item("pause", "Pause All"),
        &item("resume", "Resume All"),
        &PredefinedMenuItem::separator(),
        &item("folder", "Open Downloads Folder"),
        &item("settings", "Settings…"),
        &PredefinedMenuItem::separator(),
        &item("quit", "Quit Syphr"),
    ])?;
    let builder = TrayIconBuilder::new();
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon());
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon());
    let tray = builder
        .with_tooltip("Syphr")
        .with_menu(Box::new(menu.clone()))
        // A Mac menu bar icon opens its menu on click; elsewhere a click brings the window back.
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()?;
    Ok((tray, menu))
}

/// macOS wants a template image: the glyph alone, which it tints for light or dark menu bars.
fn icon() -> Icon {
    let full = image::load_from_memory(include_bytes!("../assets/icon/icon.png")).expect("bundled icon decodes");
    let img = if cfg!(target_os = "macos") {
        // The light glyph on the dark tile becomes black with the glyph's coverage as alpha. tray-icon draws
        // the image 22pt tall, so the crop leaves margin for a glyph of about 14pt, like the system's icons.
        let glyph = full.crop_imm(40, 40, 432, 432).to_rgba8();
        let mask = image::RgbaImage::from_fn(glyph.width(), glyph.height(), |x, y| {
            let Rgba([r, g, b, a]) = *glyph.get_pixel(x, y);
            let light = (r as u32 + g as u32 + b as u32) / 3;
            let cover = light.saturating_sub(0x30) * 255 / (0xF0 - 0x30);
            Rgba([0, 0, 0, (cover.min(255) * a as u32 / 255) as u8])
        });
        image::imageops::resize(&mask, 44, 44, FilterType::Lanczos3)
    } else {
        full.resize(32, 32, FilterType::Lanczos3).to_rgba8()
    };
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).expect("icon is valid RGBA")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{Kind, Source};

    fn download(name: &str, status: Status, total: Option<u64>, received: u64, speed: u64) -> Download {
        Download {
            id: 1,
            name: name.to_string().into(),
            url: "https://a.com/a".into(),
            kind: Kind::Video,
            source: Source::Direct,
            status,
            total_bytes: total,
            received_bytes: received,
            bytes_per_sec: speed,
            folder: "~/Downloads".into(),
            format: None,
            selector: None,
            file_name: None,
            path: None,
            fragments: Vec::new(),
            speed_history: Vec::new(),
        }
    }

    #[test]
    fn icon_builds() {
        super::icon();
    }

    #[test]
    fn rows_list_ongoing_downloads_running_first() {
        assert_eq!(row_labels(&[download("done", Status::Completed, None, 0, 0)]), [NO_DOWNLOADS]);
        let rows = row_labels(&[
            download("queued", Status::Queued, None, 0, 0),
            download("failed", Status::Failed, None, 0, 0),
            download("paused", Status::Paused, Some(200), 50, 0),
            download("a&b", Status::Downloading, Some(200), 100, 2048),
            download("no size", Status::Downloading, None, 1536, 0),
        ]);
        assert_eq!(rows, ["a&&b — 50% · 2.0 KB/s", "no size — 1.5 KB", "paused — Paused · 25%", "queued — Queued"]);
    }

    #[test]
    fn rows_cap_the_list_and_long_names() {
        let many = vec![download("x", Status::Queued, None, 0, 0); MAX_ROWS + 3];
        let rows = row_labels(&many);
        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[MAX_ROWS], "and 3 more…");

        let long = "é".repeat(MAX_NAME_CHARS + 1);
        let row = row_label(&download(&long, Status::Queued, None, 0, 0));
        assert_eq!(row, format!("{}… — Queued", "é".repeat(MAX_NAME_CHARS)));
    }
}
