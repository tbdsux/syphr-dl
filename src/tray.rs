//! The menu bar icon on macOS and the notification-area icon on Windows and Linux.

use gpui_kit::{AnyWindowHandle, App, Entity, SystemNotification};
use image::{Rgba, imageops::FilterType};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::ui::{
    app::{SyphrApp, expand_home},
    settings::{self, AppSettings},
};

/// Adds the icon and its menu; it lives until the app quits.
pub fn install(window: AnyWindowHandle, app: Entity<SyphrApp>, cx: &mut App) {
    let mut tray = match build() {
        Ok(tray) => Some(tray),
        Err(e) => return eprintln!("tray icon: {e}"),
    };

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
                "show" => crate::show_main_window(cx),
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
                _ => {}
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

fn build() -> Result<TrayIcon, Box<dyn std::error::Error>> {
    let item = |id: &str, label: &str| MenuItem::with_id(id, label, true, None);
    let menu = Menu::new();
    menu.append_items(&[
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
    Ok(builder
        .with_tooltip("Syphr")
        .with_menu(Box::new(menu))
        // A Mac menu bar icon opens its menu on click; elsewhere a click brings the window back.
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()?)
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
    #[test]
    fn icon_builds() {
        super::icon();
    }
}
