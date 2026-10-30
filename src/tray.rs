//! The menu bar icon on macOS and the notification-area icon on Windows and Linux.

use gpui_kit::App;
use image::{Rgba, imageops::FilterType};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

enum Event {
    Show,
    Quit,
}

/// Adds the icon with Show and Quit; it lives until the app quits.
pub fn install(cx: &mut App) {
    let show = MenuItem::new("Show Syphr", true, None);
    let quit = MenuItem::new("Quit Syphr", true, None);
    let mut tray = match build(&show, &quit) {
        Ok(tray) => Some(tray),
        Err(e) => return eprintln!("tray icon: {e}"),
    };

    // The handlers run on the platform's thread (a worker on Linux), so hop onto the app's.
    let (tx, rx) = async_channel::unbounded();
    let quit_id = quit.id().clone();
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        menu_tx.try_send(if e.id == quit_id { Event::Quit } else { Event::Show }).ok();
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e
            && !cfg!(target_os = "macos")
        {
            tx.try_send(Event::Show).ok();
        }
    }));
    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            cx.update(|cx| match event {
                Event::Show => crate::show_main_window(cx),
                Event::Quit => cx.quit(),
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

fn build(show: &MenuItem, quit: &MenuItem) -> Result<TrayIcon, Box<dyn std::error::Error>> {
    let menu = Menu::new();
    menu.append_items(&[show, &PredefinedMenuItem::separator(), quit])?;
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
