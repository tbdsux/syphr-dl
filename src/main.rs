// No console window behind the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

/// Cmd on macOS, Ctrl elsewhere. Spelled out rather than `secondary-` so Settings shows real keys.
macro_rules! secondary {
    ($key:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $key) } else { concat!("ctrl-", $key) }
    };
}

mod download;
mod engine;
mod tools;
mod tray;
mod ui;

use std::borrow::Cow;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

use ui::{app, settings};

actions!(
    syphr,
    [
        NewDownload,
        OpenSettings,
        Quit,
        CloseWindow,
        Minimize,
        Zoom,
        Hide,
        HideOthers,
        ShowAll,
        FocusSearch,
        BiggerText,
        SmallerText,
        ActualSize
    ]
);
// Download-list commands; bound only while the table has focus so they never eat typing in a field.
actions!(downloads, [TogglePause, Retry, CopyLinks, Remove, SelectAll, ClearSelection]);

// The default bundle only has the component icons; these Lucide extras are used for categories.
gpui_kit::assets::icon_assets!(ExtraIcons, [Film, Music, Package, FileArchive, ListChecks, Keyboard]);

/// The app icon, for `img()`.
pub const LOGO: &str = "icon/icon.png";

struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/icon/icon.png"))));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AppAssets);
    app.on_reopen(show_main_window);
    app.run(|cx| {
        gpui_kit::init(cx);
        cx.set_app_identity("dev.tbdsux.syphr-dl", "Syphr");

        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Medium.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Geist-SemiBold.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Bold.ttf").as_slice()),
            ])
            .expect("bundled fonts load");
        cx.set_global(settings::AppSettings::load());
        settings::load_bundled_themes(cx);
        settings::apply_appearance(cx);
        tools::update_ytdlp(cx);
        tray::install(cx);

        apply_shortcuts(cx);
        cx.on_action(|_: &OpenSettings, cx| settings::open_settings_window(cx));
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        // The main window handles CloseWindow itself so downloads keep running; these reach the others.
        cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window| window.remove_window()));
        cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
        cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
        cx.on_action(|_: &BiggerText, cx| settings::AppSettings::step_font_size(cx, Some(1.)));
        cx.on_action(|_: &SmallerText, cx| settings::AppSettings::step_font_size(cx, Some(-1.)));
        cx.on_action(|_: &ActualSize, cx| settings::AppSettings::step_font_size(cx, None));

        // Clicking a download notification brings the window back, as the Dock icon does.
        cx.on_system_notification_response(|_, cx| show_main_window(cx));

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.), px(720.)), cx)),
            window_min_size: Some(size(px(880.), px(520.))),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| app::SyphrApp::new(window, cx)))
            .expect("failed to open window");
        cx.activate(true);
    });
}

/// A rebindable shortcut. The user's keys live in settings under the action's name.
pub struct Shortcut {
    pub label: &'static str,
    /// Comma-separated alternatives.
    pub default: &'static str,
    context: Option<&'static str>,
    action: fn() -> Box<dyn Action>,
}

impl Shortcut {
    pub fn id(&self) -> &'static str {
        (self.action)().name()
    }
}

// The "DataTable" ones only fire while the download list has focus, so they never eat typing in a field.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut { label: "New download", default: secondary!("n"), context: None, action: || Box::new(NewDownload) },
    Shortcut { label: "Search", default: secondary!("f"), context: None, action: || Box::new(FocusSearch) },
    Shortcut { label: "Settings", default: secondary!(","), context: None, action: || Box::new(OpenSettings) },
    Shortcut { label: "Close window", default: secondary!("w"), context: None, action: || Box::new(CloseWindow) },
    Shortcut { label: "Minimize", default: secondary!("m"), context: None, action: || Box::new(Minimize) },
    Shortcut { label: "Hide Syphr", default: "cmd-h", context: None, action: || Box::new(Hide) },
    Shortcut { label: "Hide others", default: "alt-cmd-h", context: None, action: || Box::new(HideOthers) },
    Shortcut { label: "Quit", default: secondary!("q"), context: None, action: || Box::new(Quit) },
    Shortcut { label: "Make text bigger", default: secondary!("="), context: None, action: || Box::new(BiggerText) },
    Shortcut { label: "Make text smaller", default: secondary!("-"), context: None, action: || Box::new(SmallerText) },
    Shortcut { label: "Actual size", default: secondary!("0"), context: None, action: || Box::new(ActualSize) },
    Shortcut {
        label: "Pause or resume",
        default: "space",
        context: Some("DataTable"),
        action: || Box::new(TogglePause),
    },
    Shortcut {
        label: "Retry failed",
        default: secondary!("r"),
        context: Some("DataTable"),
        action: || Box::new(Retry),
    },
    Shortcut {
        label: "Copy links",
        default: secondary!("c"),
        context: Some("DataTable"),
        action: || Box::new(CopyLinks),
    },
    Shortcut { label: "Remove", default: "backspace, delete", context: Some("DataTable"), action: || Box::new(Remove) },
    Shortcut {
        label: "Select all",
        default: secondary!("a"),
        context: Some("DataTable"),
        action: || Box::new(SelectAll),
    },
    // Overrides the table's own Cancel, which only clears its keyboard cursor.
    Shortcut {
        label: "Clear selection",
        default: "escape",
        context: Some("DataTable"),
        action: || Box::new(ClearSelection),
    },
];

/// Parses `keys` into bindings for `shortcut`; empty means turned off.
pub fn shortcut_bindings(shortcut: &Shortcut, keys: &str, cx: &App) -> Result<Vec<KeyBinding>, InvalidKeystrokeError> {
    keys.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| {
            let context = shortcut.context.map(|c| KeyBindingContextPredicate::parse(c).expect("valid context").into());
            KeyBinding::load(k, (shortcut.action)(), context, false, None, cx)
        })
        .collect()
}

/// Rebinds every shortcut from settings, keeping the components' own bindings. Invalid keys fall back to the default.
pub fn apply_shortcuts(cx: &mut App) {
    let ours: Vec<&str> = SHORTCUTS.iter().map(Shortcut::id).collect();
    let mut bindings: Vec<KeyBinding> =
        cx.key_bindings().borrow().bindings().filter(|b| !ours.contains(&b.action().name())).cloned().collect();
    for shortcut in SHORTCUTS {
        let keys = settings::AppSettings::get(cx).shortcut(shortcut);
        let parsed = shortcut_bindings(shortcut, &keys, cx)
            .or_else(|_| shortcut_bindings(shortcut, shortcut.default, cx))
            .expect("default shortcuts parse");
        // Added last so they win over component bindings on the same keys.
        bindings.extend(parsed);
    }
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    // Menus show the bound keys, so rebuild them too.
    cx.set_menus(menus());
    gpui_kit::component::GlobalState::global_mut(cx).set_app_menus(menus().into_iter().map(Menu::owned).collect());
}

fn show_main_window(cx: &mut App) {
    cx.activate(true);
    if let Some(window) = cx.windows().first() {
        window.update(cx, |_, window, _| window.activate_window()).ok();
    }
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}

/// The macOS menu bar, and the in-window menu bar on Windows and Linux.
fn menus() -> Vec<Menu> {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    let edit = Menu::new("Edit").items([
        MenuItem::os_action("Undo", Undo, OsAction::Undo),
        MenuItem::os_action("Redo", Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", Cut, OsAction::Cut),
        MenuItem::os_action("Copy", Copy, OsAction::Copy),
        MenuItem::os_action("Paste", Paste, OsAction::Paste),
        MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
    ]);
    let view = Menu::new("View").items([
        MenuItem::action("Actual Size", ActualSize),
        MenuItem::action("Make Text Bigger", BiggerText),
        MenuItem::action("Make Text Smaller", SmallerText),
    ]);
    if !cfg!(target_os = "macos") {
        return vec![
            Menu::new("File").items([
                MenuItem::action("New Download…", NewDownload),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindow),
                MenuItem::action("Quit", Quit),
            ]),
            edit,
            view,
        ];
    }
    vec![
        Menu::new("Syphr").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Syphr", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Syphr", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Download…", NewDownload),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        edit,
        view,
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]),
    ]
}

#[cfg(test)]
mod tests {
    use super::SHORTCUTS;
    use gpui_kit::{Keystroke, OperatingSystem};

    #[test]
    fn default_shortcuts_parse_and_ids_are_unique() {
        for s in SHORTCUTS {
            for k in s.default.split(',') {
                Keystroke::parse_with_os(k.trim(), OperatingSystem::Mac).unwrap();
            }
            assert_eq!(SHORTCUTS.iter().filter(|o| o.id() == s.id()).count(), 1, "{}", s.id());
        }
    }
}
