use std::{collections::BTreeMap, path::PathBuf, process::Command};

use gpui_kit::component::{
    Disableable, Icon, IconName, Theme, ThemeRegistry, TitleBar, WindowExt,
    button::Button,
    h_flex,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::download::format_bytes;

pub const FONT_FAMILY: &str = "Geist";
pub const SYSTEM_THEME: &str = "System";
/// Smallest and largest base text size in points; everything else scales with it.
pub const TEXT_SIZES: (f64, f64) = (11., 20.);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: String,
    pub font_size: f64,
    pub download_dir: String,
    pub max_concurrent: f64,
    /// 0 means unlimited.
    pub speed_limit_kbps: f64,
    /// Parallel connections per download: byte-range parts for direct links, `-N` for yt-dlp.
    pub fragments: f64,
    pub ytdlp_path: String,
    /// yt-dlp needs it to merge separate video and audio streams.
    pub ffmpeg_path: String,
    pub notify_on_complete: bool,
    /// Opening the link field fills it with a copied link.
    pub auto_paste: bool,
    pub show_quick_download: bool,
    /// Keys the user changed, by action name; the rest use `Shortcut::default`.
    pub shortcuts: BTreeMap<String, String>,
}

impl Global for AppSettings {}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: SYSTEM_THEME.into(),
            font_size: 13.,
            download_dir: home().join("Downloads").display().to_string(),
            max_concurrent: 3.,
            speed_limit_kbps: 0.,
            fragments: 8.,
            ytdlp_path: "yt-dlp".into(),
            ffmpeg_path: "ffmpeg".into(),
            notify_on_complete: true,
            auto_paste: true,
            show_quick_download: true,
            shortcuts: BTreeMap::new(),
        }
    }
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

/// Syphr's own folder: settings, and the tools it installs.
pub(crate) fn data_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
    };
    base.join("Syphr")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

impl AppSettings {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn load() -> Self {
        let path = settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_else(|err| {
            // The next change overwrites the file, so say why the user's settings vanished.
            eprintln!("Ignoring unreadable settings in {}: {err}", path.display());
            Self::default()
        })
    }

    pub fn shortcut(&self, shortcut: &crate::Shortcut) -> String {
        self.shortcuts.get(shortcut.id()).cloned().unwrap_or_else(|| shortcut.default.into())
    }

    fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        std::fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
        std::fs::write(path, serde_json::to_string_pretty(self)?)
    }

    /// Changes a setting, writes it to disk and re-applies theme and font.
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut Self)) {
        edit(cx.global_mut::<Self>());
        if let Err(err) = cx.global::<Self>().save() {
            // shortcut: no window here to show it in; surface it in the UI once settings errors matter.
            eprintln!("Couldn't save settings to {}: {err}", settings_path().display());
        }
        apply_appearance(cx);
    }

    /// Steps the base text size by `delta` points, or back to the default for `None`.
    pub fn step_font_size(cx: &mut App, delta: Option<f64>) {
        Self::update(cx, |s| {
            s.font_size = match delta {
                Some(d) => (s.font_size + d).clamp(TEXT_SIZES.0, TEXT_SIZES.1),
                None => Self::default().font_size,
            }
        });
    }
}

pub fn load_bundled_themes(cx: &mut App) {
    let themes = [
        include_str!("../../assets/themes/catppuccin.json"),
        include_str!("../../assets/themes/everforest.json"),
        include_str!("../../assets/themes/flexoki.json"),
        include_str!("../../assets/themes/gruvbox.json"),
        include_str!("../../assets/themes/solarized.json"),
        include_str!("../../assets/themes/tokyonight.json"),
    ];
    let registry = ThemeRegistry::global_mut(cx);
    for theme in themes {
        registry.load_themes_from_str(theme).expect("bundled theme is valid");
    }
}

pub fn apply_appearance(cx: &mut App) {
    let settings = AppSettings::get(cx).clone();
    let config = ThemeRegistry::global(cx).themes().get(settings.theme.as_str()).cloned();
    match config {
        Some(config) => Theme::update(cx, |theme| theme.apply_config(&config)),
        None => Theme::sync_system_appearance(None, cx),
    }
    Theme::update(cx, |theme| {
        theme.font_family = FONT_FAMILY.into();
        theme.font_size = px(settings.font_size as f32);
    });
    cx.refresh_windows();
}

struct SettingsWindow(Option<AnyWindowHandle>);
impl Global for SettingsWindow {}

pub fn open_settings_window(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsWindow>().and_then(|w| w.0)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(560.)), cx)),
        window_min_size: Some(size(px(600.), px(420.))),
        ..TitleBar::window_options()
    };
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| SettingsView)) {
        cx.set_global(SettingsWindow(Some(handle)));
    }
}

struct SettingsView;

fn number(min: f64, max: f64, step: f64) -> NumberFieldOptions {
    NumberFieldOptions { min, max, step }
}

/// Both are required for site downloads: yt-dlp fetches, ffmpeg merges video with audio.
fn check_tools(window: &mut Window, cx: &mut App) {
    let settings = AppSettings::get(cx);
    let tools = [
        ("yt-dlp", crate::tools::resolve(&settings.ytdlp_path), "--version"),
        ("ffmpeg", crate::tools::resolve(&settings.ffmpeg_path), "-version"),
    ];
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        for (name, bin, flag) in tools {
            let result =
                cx.background_spawn(async move { Command::new(&bin).arg(flag).output().map(|o| (bin, o)) }).await;
            let message = match result {
                Ok((bin, out)) if out.status.success() => {
                    // ffmpeg prints "ffmpeg version 7.1 Copyright…"; keep only the version.
                    let first = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string();
                    let version = first
                        .trim_start_matches("ffmpeg version ")
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    format!("{bin} {version} is ready")
                }
                Ok((bin, _)) => format!("{bin} ran but reported an error"),
                Err(_) => format!("{name} wasn't found. Use Install required tools, or set its full path."),
            };
            cx.update_window(handle, |_, window, cx| window.push_notification(message, cx)).ok();
        }
    })
    .detach();
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let defaults = AppSettings::default();
        let mut themes: Vec<(SharedString, SharedString)> =
            vec![(SYSTEM_THEME.into(), "Match system (Default Light / Dark)".into())];
        themes.extend(ThemeRegistry::global(cx).sorted_themes().into_iter().map(|t| (t.name.clone(), t.name.clone())));

        let appearance =
            SettingPage::new("Appearance").icon(IconName::Palette).default_open(true).resettable(true).group(
                SettingGroup::new().items([
                    SettingItem::new(
                        "Theme",
                        SettingField::scrollable_dropdown(
                            themes,
                            |cx: &App| AppSettings::get(cx).theme.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                AppSettings::update(cx, |s| s.theme = value.to_string())
                            },
                        )
                        .default_value(defaults.theme.clone()),
                    )
                    .description("Colors for every Syphr window."),
                    SettingItem::new(
                        "Text size",
                        // A dropdown, not a number field: a focused input inside the settings list re-requests
                        // autoscroll as the layout grows, and gpui then prepaints it twice in one frame.
                        SettingField::scrollable_dropdown(
                            (TEXT_SIZES.0 as u32..=TEXT_SIZES.1 as u32)
                                .map(|pt| (pt.to_string().into(), format!("{pt} pt").into()))
                                .collect(),
                            |cx: &App| AppSettings::get(cx).font_size.to_string().into(),
                            |value: SharedString, cx: &mut App| {
                                if let Ok(pt) = value.parse() {
                                    AppSettings::update(cx, |s| s.font_size = pt)
                                }
                            },
                        )
                        .default_value(SharedString::from(defaults.font_size.to_string())),
                    )
                    .description(
                        "Base size in points; spacing and controls scale with it. The View menu changes it too.",
                    ),
                ]),
            );

        let downloads = SettingPage::new("Downloads").icon(IconName::HardDrive).resettable(true).group(
            SettingGroup::new().items([
                SettingItem::new(
                    "Save to",
                    SettingField::input(
                        |cx: &App| AppSettings::get(cx).download_dir.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            AppSettings::update(cx, |s| s.download_dir = value.to_string())
                        },
                    )
                    .default_value(defaults.download_dir.clone()),
                )
                .description("Default folder for new downloads."),
                SettingItem::new(
                    "Simultaneous downloads",
                    SettingField::number_input(
                        number(1., 10., 1.),
                        |cx: &App| AppSettings::get(cx).max_concurrent,
                        |value: f64, cx: &mut App| AppSettings::update(cx, |s| s.max_concurrent = value),
                    )
                    .default_value(defaults.max_concurrent),
                )
                .description("Others wait in the queue."),
                SettingItem::new(
                    "Speed limit (KB/s)",
                    SettingField::number_input(
                        number(0., 1_000_000., 100.),
                        |cx: &App| AppSettings::get(cx).speed_limit_kbps,
                        |value: f64, cx: &mut App| AppSettings::update(cx, |s| s.speed_limit_kbps = value),
                    )
                    .default_value(defaults.speed_limit_kbps),
                )
                .description("Shared by all downloads. 0 means no limit."),
                SettingItem::new(
                    "Connections per download",
                    SettingField::number_input(
                        number(1., 32., 1.),
                        |cx: &App| AppSettings::get(cx).fragments,
                        |value: f64, cx: &mut App| AppSettings::update(cx, |s| s.fragments = value),
                    )
                    .default_value(defaults.fragments),
                )
                .description(
                    "Parts fetched at once. Servers without range support always use one. Applies to new downloads.",
                ),
                SettingItem::new(
                    "Notify when a download finishes",
                    SettingField::switch(
                        |cx: &App| AppSettings::get(cx).notify_on_complete,
                        |value: bool, cx: &mut App| AppSettings::update(cx, |s| s.notify_on_complete = value),
                    )
                    .default_value(defaults.notify_on_complete),
                ),
                SettingItem::new(
                    "Paste copied links automatically",
                    SettingField::switch(
                        |cx: &App| AppSettings::get(cx).auto_paste,
                        |value: bool, cx: &mut App| AppSettings::update(cx, |s| s.auto_paste = value),
                    )
                    .default_value(defaults.auto_paste),
                )
                .description("Opening the link field (⌘N) fills it with the last copied link."),
                SettingItem::new(
                    "Show Quick download",
                    SettingField::switch(
                        |cx: &App| AppSettings::get(cx).show_quick_download,
                        |value: bool, cx: &mut App| AppSettings::update(cx, |s| s.show_quick_download = value),
                    )
                    .default_value(defaults.show_quick_download),
                )
                .description("A sidebar button that starts a download from the last copied link."),
                SettingItem::new(
                    "Unfinished downloads",
                    SettingField::render(|_, _, _| {
                        // Re-measured on each render; the folder holds a handful of files.
                        let (root, bytes) = crate::engine::partial_usage();
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(if bytes == 0 { "None".into() } else { format_bytes(bytes) })
                            .child(
                                Button::new("reveal-partial")
                                    .outline()
                                    .label("Show in folder")
                                    .disabled(bytes == 0)
                                    .on_click(move |_, window, cx| {
                                        if let Err(e) = crate::ui::app::reveal(&root) {
                                            window.push_notification(format!("Couldn't open the folder: {e}"), cx);
                                        }
                                    }),
                            )
                    }),
                )
                .description("Partial files kept so paused downloads can resume. Removed downloads leave theirs here."),
            ]),
        );

        let ytdlp = SettingPage::new("YouTube & sites").icon(IconName::Globe).resettable(true).group(
            SettingGroup::new().items([
                SettingItem::new(
                    "yt-dlp path",
                    SettingField::input(
                        |cx: &App| AppSettings::get(cx).ytdlp_path.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            AppSettings::update(cx, |s| s.ytdlp_path = value.to_string())
                        },
                    )
                    .default_value(defaults.ytdlp_path.clone()),
                )
                .description("Command name or full path. youtube-dl works too."),
                SettingItem::new(
                    "ffmpeg path",
                    SettingField::input(
                        |cx: &App| AppSettings::get(cx).ffmpeg_path.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            AppSettings::update(cx, |s| s.ffmpeg_path = value.to_string())
                        },
                    )
                    .default_value(defaults.ffmpeg_path.clone()),
                )
                .description("Required to join video and audio. Only “Audio only” downloads work without it."),
                SettingItem::new(
                    "Check installation",
                    SettingField::render(|_, _, cx| {
                        h_flex().gap_2().children(crate::tools::install_button(cx)).child(
                            Button::new("check-tools")
                                .label("Check yt-dlp & ffmpeg")
                                .on_click(|_, window, cx| check_tools(window, cx)),
                        )
                    }),
                )
                .description("Syphr can download its own copies of both. Your own installs are used first."),
            ]),
        );

        let shortcuts = SettingPage::new("Shortcuts").icon(Icon::new(gpui_kit::assets::IconName::Keyboard)).resettable(true).group(
            SettingGroup::new()
                .description(
                    "Combine cmd, ctrl, alt or shift with a key, as in cmd-shift-n. Separate alternatives with a comma; \
                     leave a field empty to turn it off. List shortcuts work while the download list has focus.",
                )
                .items(crate::SHORTCUTS.iter().map(|shortcut| {
                    let item = SettingItem::new(
                        shortcut.label,
                        SettingField::input(
                            move |cx: &App| AppSettings::get(cx).shortcut(shortcut).into(),
                            move |value: SharedString, cx: &mut App| {
                                AppSettings::update(cx, |s| {
                                    if value.as_ref() == shortcut.default {
                                        s.shortcuts.remove(shortcut.id());
                                    } else {
                                        s.shortcuts.insert(shortcut.id().into(), value.to_string());
                                    }
                                });
                                crate::apply_shortcuts(cx);
                            },
                        )
                        .default_value(SharedString::from(shortcut.default)),
                    );
                    let keys = AppSettings::get(cx).shortcut(shortcut);
                    match crate::shortcut_bindings(shortcut, &keys, cx) {
                        Ok(_) => item,
                        Err(_) => item.description(format!("Not a valid shortcut, so {} is used.", shortcut.default)),
                    }
                })),
        );

        v_flex().size_full().child(TitleBar::new().child("Settings")).child(
            div().flex_1().min_h_0().child(Settings::new("settings").pages([appearance, downloads, ytdlp, shortcuts])),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::AppSettings;

    #[test]
    fn settings_round_trip_and_fill_missing_fields() {
        let s = AppSettings { max_concurrent: 5., ..Default::default() };
        let back: AppSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let partial: AppSettings = serde_json::from_str(r#"{"font_size": 14}"#).unwrap();
        assert_eq!(partial.font_size, 14.);
        assert_eq!(partial.ytdlp_path, "yt-dlp");
        assert_eq!(partial.ffmpeg_path, "ffmpeg");
        assert_eq!(partial.fragments, 8.);
    }
}
