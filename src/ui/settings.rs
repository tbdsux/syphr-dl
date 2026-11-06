use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Command,
};

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Theme, ThemeRegistry, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    link::Link,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};
use gpui_kit::*;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::download::format_bytes;

pub const FONT_FAMILY: &str = "DM Sans";
pub const SYSTEM_THEME: &str = "System";
/// Smallest and largest base text size in points; everything else scales with it.
pub const TEXT_SIZES: (f64, f64) = (11., 20.);
const PROXY_NONE: &str = "none";

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
    /// URL scheme of the proxy for all of Syphr's traffic, or `PROXY_NONE`.
    pub proxy_type: String,
    pub proxy_host: String,
    pub proxy_port: f64,
    pub proxy_username: String,
    pub proxy_password: String,
    pub ytdlp_path: String,
    /// yt-dlp needs it to merge separate video and audio streams.
    pub ffmpeg_path: String,
    /// Video codec picked first at each resolution; see `ytdlp::Options`.
    pub ytdlp_codec: String,
    pub ytdlp_container: String,
    /// Extra yt-dlp arguments as typed, split like a shell line.
    pub ytdlp_args: String,
    pub notify_on_complete: bool,
    /// Opening the link field fills it with a copied link.
    pub auto_paste: bool,
    pub show_quick_download: bool,
    pub check_updates: bool,
    /// Download list columns turned off, by column id.
    pub hidden_columns: BTreeSet<String>,
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
            proxy_type: PROXY_NONE.into(),
            proxy_host: String::new(),
            proxy_port: 8080.,
            proxy_username: String::new(),
            proxy_password: String::new(),
            ytdlp_path: "yt-dlp".into(),
            ffmpeg_path: "ffmpeg".into(),
            ytdlp_codec: "any".into(),
            ytdlp_container: "auto".into(),
            ytdlp_args: String::new(),
            notify_on_complete: true,
            auto_paste: true,
            show_quick_download: true,
            check_updates: true,
            hidden_columns: BTreeSet::new(),
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

    /// The proxy as a link, empty when off. An error means no request should run, not that it should go without one.
    pub fn proxy_url(&self) -> Result<String, String> {
        if self.proxy_type == PROXY_NONE {
            return Ok(String::new());
        }
        // Checked here because reqwest ignores a proxy it can't read, connecting directly instead.
        let invalid = || "The proxy's host or type isn't valid. Check them in Settings → Proxy.".to_string();
        let host = self.proxy_host.trim();
        let known = matches!(self.proxy_type.as_str(), "http" | "https" | "socks5" | "socks5h");
        // Only a bare host: a pasted link would otherwise parse with its path swallowing the port.
        if !known || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:[]".contains(c)) {
            return Err(invalid());
        }
        let address = format!("{}://{host}:{}", self.proxy_type, self.proxy_port as u16);
        let mut url = Url::parse(&address).map_err(|_| invalid())?;
        // These percent-encode, so any character works in a password.
        url.set_username(&self.proxy_username).map_err(|_| invalid())?;
        if !self.proxy_password.is_empty() {
            url.set_password(Some(&self.proxy_password)).map_err(|_| invalid())?;
        }
        Ok(url.to_string())
    }

    pub fn ytdlp_options(&self) -> crate::engine::ytdlp::Options {
        crate::engine::ytdlp::Options {
            codec: self.ytdlp_codec.clone(),
            container: self.ytdlp_container.clone(),
            extra_args: crate::engine::ytdlp::split_args(&self.ytdlp_args),
        }
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
        // Window chrome takes a hint of the theme's accent, so it reads as a frame around the content panel.
        theme.sidebar = theme.sidebar.blend(theme.primary.opacity(0.08));
        // Download rows are bars that stack; rules between them would cut the stack up. Resize handles still show on hover.
        theme.table_row_border = transparent_black();
        // Some themes tint selection in a color unrelated to their accent; a selected row should read as the accent.
        theme.table_active = theme.primary.opacity(0.16);
        theme.table_head = transparent_black();
        theme.table = transparent_black();
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
        window_min_size: Some(size(px(720.), px(520.))),
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

/// A row's value on the About page: optional plain text, then links out.
fn about_field(
    text: Option<&'static str>,
    links: &'static [(&'static str, &'static str)],
) -> SettingField<SharedString> {
    SettingField::render(move |_, _, cx| {
        h_flex()
            .gap_3()
            .children(text.map(|text| div().text_color(cx.theme().muted_foreground).child(text)))
            .children(links.iter().map(|&(label, url)| Link::new(url).href(url).child(label)))
    })
}

/// A path input with a button that opens the file picker; the kit's own input field has no slot for one.
fn tool_path_field(
    title: &'static str,
    get: fn(&AppSettings) -> &String,
    set: fn(&mut AppSettings, String),
    default: String,
) -> SettingField<SharedString> {
    let reset_to = default.clone();
    SettingField::render(move |_, window, cx| {
        let value: SharedString = get(AppSettings::get(cx)).clone().into();
        let input = window.use_keyed_state(title, cx, |window, cx| {
            cx.subscribe_self(move |input: &mut InputState, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let path = input.value().to_string();
                    AppSettings::update(cx, |s| set(s, path));
                }
            })
            .detach();
            InputState::new(window, cx).default_value(value.clone())
        });
        if input.read(cx).value() != value {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        h_flex().w_full().gap_2().child(Input::new(&input).aria_label(title).flex_1()).child(
            Button::new(title).outline().label("Select…").on_click(move |_, window, cx| select_tool(set, window, cx)),
        )
    })
    .on_reset(
        move |cx| get(AppSettings::get(cx)) != &default,
        move |_, cx| AppSettings::update(cx, |s| set(s, reset_to.clone())),
    )
}

fn update_proxy(cx: &mut App, edit: impl FnOnce(&mut AppSettings)) {
    AppSettings::update(cx, edit);
    crate::engine::set_proxy(AppSettings::get(cx).proxy_url());
}

fn update_ytdlp(cx: &mut App, edit: impl FnOnce(&mut AppSettings)) {
    AppSettings::update(cx, edit);
    crate::engine::ytdlp::set_options(AppSettings::get(cx).ytdlp_options());
}

/// `SettingField::input` can't mask its text.
fn proxy_password_field() -> SettingField<SharedString> {
    SettingField::render(|_, window, cx| {
        let value: SharedString = AppSettings::get(cx).proxy_password.clone().into();
        let input = window.use_keyed_state("proxy-password", cx, |window, cx| {
            cx.subscribe_self(|input: &mut InputState, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let password = input.value().to_string();
                    update_proxy(cx, |s| s.proxy_password = password);
                }
            })
            .detach();
            InputState::new(window, cx).masked(true).default_value(value.clone())
        });
        if input.read(cx).value() != value {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        Input::new(&input).aria_label("Proxy password").mask_toggle()
    })
    .on_reset(
        |cx| !AppSettings::get(cx).proxy_password.is_empty(),
        |_, cx| update_proxy(cx, |s| s.proxy_password.clear()),
    )
}

fn select_tool(set: fn(&mut AppSettings, String), window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Select".into()),
    });
    cx.spawn(async move |cx| match picked.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.first() {
                cx.update(|cx| AppSettings::update(cx, |s| set(s, path.display().to_string())));
            }
        }
        Ok(Ok(None)) | Err(_) => {}
        Ok(Err(e)) => {
            let message = format!("Couldn't open the file picker: {e}. Type the full path instead.");
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

        let appearance = SettingPage::new("Appearance")
            .icon(IconName::Palette)
            .default_open(true)
            .resettable(true)
            .group(
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
            )
            .group(SettingGroup::new().title("Download list columns").items(
                crate::ui::downloads_table::OPTIONAL_COLUMNS.iter().map(|&(id, label)| {
                    SettingItem::new(
                        label,
                        SettingField::switch(
                            move |cx: &App| !AppSettings::get(cx).hidden_columns.contains(id),
                            move |show: bool, cx: &mut App| {
                                AppSettings::update(cx, |s| {
                                    if show {
                                        s.hidden_columns.remove(id);
                                    } else {
                                        s.hidden_columns.insert(id.into());
                                    }
                                })
                            },
                        )
                        .default_value(true),
                    )
                }),
            ));

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
                // Paths are long: full width shows more of them, and the kit's fixed field overflows narrow windows.
                .layout(Axis::Vertical)
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
                .description("Partial files kept so paused downloads can resume. Removing a download deletes its own."),
            ]),
        );

        let mut proxy_items = vec![
            SettingItem::new(
                "Type",
                SettingField::dropdown(
                    vec![
                        (PROXY_NONE.into(), "None".into()),
                        ("http".into(), "HTTP".into()),
                        ("https".into(), "HTTPS".into()),
                        ("socks5".into(), "SOCKS5".into()),
                        ("socks5h".into(), "SOCKS5 with proxy DNS".into()),
                    ],
                    |cx: &App| AppSettings::get(cx).proxy_type.clone().into(),
                    |value: SharedString, cx: &mut App| update_proxy(cx, |s| s.proxy_type = value.to_string()),
                )
                .default_value(defaults.proxy_type.clone()),
            )
            .description(
                "HTTP and SOCKS5 suit most providers. Choose HTTPS only if yours says it encrypts the connection to the \
                 proxy. SOCKS5 with proxy DNS also sends site lookups through the proxy.",
            ),
        ];
        if AppSettings::get(cx).proxy_type != PROXY_NONE {
            let host = SettingItem::new(
                "Host",
                SettingField::input(
                    |cx: &App| AppSettings::get(cx).proxy_host.clone().into(),
                    |value: SharedString, cx: &mut App| update_proxy(cx, |s| s.proxy_host = value.trim().to_string()),
                )
                .default_value(defaults.proxy_host.clone()),
            )
            .layout(Axis::Vertical);
            proxy_items.extend([
                match AppSettings::get(cx).proxy_url() {
                    Ok(_) => host.description("Name or IP address only, such as proxy.example.com."),
                    Err(message) => host.description(message),
                },
                SettingItem::new(
                    "Port",
                    SettingField::number_input(
                        number(1., 65535., 1.),
                        |cx: &App| AppSettings::get(cx).proxy_port,
                        |value: f64, cx: &mut App| update_proxy(cx, |s| s.proxy_port = value),
                    )
                    .default_value(defaults.proxy_port),
                ),
                SettingItem::new(
                    "Username",
                    SettingField::input(
                        |cx: &App| AppSettings::get(cx).proxy_username.clone().into(),
                        |value: SharedString, cx: &mut App| update_proxy(cx, |s| s.proxy_username = value.to_string()),
                    )
                    .default_value(defaults.proxy_username.clone()),
                )
                .layout(Axis::Vertical)
                .description("Leave empty if the proxy doesn't ask for one."),
                SettingItem::new("Password", proxy_password_field())
                    .layout(Axis::Vertical)
                    .description("Saved unencrypted in Syphr's settings file."),
            ]);
        }
        let proxy =
            SettingPage::new("Proxy").icon(Icon::new(gpui_kit::assets::IconName::Network)).resettable(true).group(
                SettingGroup::new()
                    .description("Used for downloads, tool installs and update checks. Applies to new downloads.")
                    .items(proxy_items),
            );

        let ytdlp = SettingPage::new("YouTube & sites").icon(IconName::Globe).resettable(true).group(
            SettingGroup::new().items([
                SettingItem::new(
                    "yt-dlp path",
                    tool_path_field(
                        "yt-dlp path",
                        |s| &s.ytdlp_path,
                        |s, path| s.ytdlp_path = path,
                        defaults.ytdlp_path.clone(),
                    ),
                )
                .layout(Axis::Vertical)
                .description("Command name or full path. youtube-dl works too."),
                SettingItem::new(
                    "ffmpeg path",
                    tool_path_field(
                        "ffmpeg path",
                        |s| &s.ffmpeg_path,
                        |s, path| s.ffmpeg_path = path,
                        defaults.ffmpeg_path.clone(),
                    ),
                )
                .layout(Axis::Vertical)
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
        )
        .group(
            SettingGroup::new()
                .title("Formats")
                .description("Applies to links looked up from now on.")
                .items([
                    SettingItem::new(
                        "Preferred video codec",
                        SettingField::dropdown(
                            vec![
                                ("any".into(), "No preference".into()),
                                ("h264".into(), "H.264 (plays everywhere)".into()),
                                ("vp9".into(), "VP9".into()),
                                ("av1".into(), "AV1 (smallest files)".into()),
                            ],
                            |cx: &App| AppSettings::get(cx).ytdlp_codec.clone().into(),
                            |value: SharedString, cx: &mut App| update_ytdlp(cx, |s| s.ytdlp_codec = value.to_string()),
                        )
                        .default_value(defaults.ytdlp_codec.clone()),
                    )
                    .description(
                        "Picked first at each resolution when the site offers it. YouTube has H.264 only up to 1080p, \
                         so higher resolutions still come in VP9 or AV1.",
                    ),
                    SettingItem::new(
                        "Video file type",
                        SettingField::dropdown(
                            vec![
                                ("auto".into(), "Automatic".into()),
                                ("mp4".into(), "MP4".into()),
                                ("mkv".into(), "MKV".into()),
                            ],
                            |cx: &App| AppSettings::get(cx).ytdlp_container.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                update_ytdlp(cx, |s| s.ytdlp_container = value.to_string())
                            },
                        )
                        .default_value(defaults.ytdlp_container.clone()),
                    )
                    .description(
                        "Automatic keeps the site's format, such as WebM. Others repackage the file with ffmpeg without \
                         re-encoding. Some players can't open VP9 or AV1 inside MP4.",
                    ),
                    SettingItem::new(
                        "Extra yt-dlp options",
                        SettingField::input(
                            |cx: &App| AppSettings::get(cx).ytdlp_args.clone().into(),
                            |value: SharedString, cx: &mut App| update_ytdlp(cx, |s| s.ytdlp_args = value.to_string()),
                        )
                        .default_value(defaults.ytdlp_args.clone()),
                    )
                    .layout(Axis::Vertical)
                    .description(
                        "Added to every lookup and download, as in --embed-metadata --cookies-from-browser chrome. \
                         Quote values with spaces. Syphr's own options win where they clash.",
                    ),
                ]),
        );

        let updates = SettingPage::new("Updates")
            .icon(Icon::new(gpui_kit::assets::IconName::CircleArrowUp))
            .resettable(true)
            .group(SettingGroup::new().items([
                SettingItem::new(
                    format!("Syphr {}", env!("CARGO_PKG_VERSION")),
                    SettingField::render(|_, _, cx| {
                        let busy = crate::update::busy(cx);
                        let button = match crate::update::available(cx) {
                            Some(release) => Button::new("install-update")
                                .primary()
                                .label(busy.clone().unwrap_or(format!("Update to {}", release.tag).into()))
                                .on_click(|_, window, cx| crate::update::install(window.window_handle(), cx)),
                            None => Button::new("check-update")
                                .label(busy.clone().unwrap_or("Check for updates".into()))
                                .on_click(|_, window, cx| crate::update::check(Some(window.window_handle()), cx)),
                        };
                        button.loading(busy.is_some())
                    }),
                )
                .description("Updates come from GitHub releases and are checked against their SHA-256 sums. Syphr restarts to finish."),
                SettingItem::new(
                    "Check for updates automatically",
                    SettingField::switch(
                        |cx: &App| AppSettings::get(cx).check_updates,
                        |value: bool, cx: &mut App| AppSettings::update(cx, |s| s.check_updates = value),
                    )
                    .default_value(defaults.check_updates),
                )
                .description("At launch and every few hours. Beta builds are offered betas too."),
            ]));

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
                        .default_value(SharedString::from(shortcut.default))
                        // Narrower than the kit's default, which overflows the row at the smaller window sizes.
                        .w(rems(10.)),
                    );
                    let keys = AppSettings::get(cx).shortcut(shortcut);
                    match crate::shortcut_bindings(shortcut, &keys, cx) {
                        Ok(_) => item,
                        Err(_) => item.description(format!("Not a valid shortcut, so {} is used.", shortcut.default)),
                    }
                })),
        );

        let mut third_party = SettingGroup::new().title("Third-party software");
        // Only what this download ships: the regular build leaves yt-dlp and ffmpeg to their upstream sources.
        if crate::tools::is_full_build() {
            third_party = third_party
                .description("yt-dlp and ffmpeg come with this download and keep their own licenses. Syphr runs them as separate programs.")
                .items([
                    SettingItem::new("yt-dlp", about_field(None, &[("GPLv3+", "https://github.com/yt-dlp/yt-dlp#licensing"), ("Source", "https://github.com/yt-dlp/yt-dlp")])),
                    SettingItem::new("ffmpeg", about_field(None, &[("GPLv3", "https://www.ffmpeg.org/legal.html"), ("Source", "https://www.ffmpeg.org/download.html#get-sources")])),
                ]);
        }
        third_party = third_party.item(SettingItem::new(
            "DM Sans",
            about_field(
                None,
                &[("SIL Open Font License", "https://github.com/tbdsux/syphr-dl/blob/main/assets/fonts/OFL.txt")],
            ),
        ));
        let about = SettingPage::new("About")
            .icon(IconName::Info)
            .resettable(false)
            .description(env!("CARGO_PKG_DESCRIPTION"))
            .group(SettingGroup::new().items([
                SettingItem::new(
                    "Version",
                    about_field(
                        Some(env!("CARGO_PKG_VERSION")),
                        &[(
                            "Release notes",
                            concat!("https://github.com/tbdsux/syphr-dl/releases/tag/v", env!("CARGO_PKG_VERSION")),
                        )],
                    ),
                ),
                SettingItem::new(
                    "License",
                    about_field(None, &[("MIT", "https://github.com/tbdsux/syphr-dl/blob/main/LICENSE")]),
                ),
                SettingItem::new("Source code", about_field(None, &[("GitHub", "https://github.com/tbdsux/syphr-dl")])),
            ]))
            .group(third_party);

        let chrome = cx.theme().sidebar;
        v_flex().size_full().child(TitleBar::new().bg(chrome).border_color(chrome).child("Settings")).child(
            div().flex_1().min_h_0().child(
                Settings::new("settings")
                    .sidebar_width(cx.theme().font_size * 18.)
                    .pages([appearance, downloads, proxy, ytdlp, updates, shortcuts, about]),
            ),
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

    #[test]
    fn builds_proxy_links() {
        let proxy = |proxy_type: &str, host: &str| AppSettings {
            proxy_type: proxy_type.into(),
            proxy_host: host.into(),
            proxy_port: 1080.,
            ..Default::default()
        };
        assert_eq!(AppSettings::default().proxy_url().unwrap(), "");
        assert_eq!(proxy("socks5h", "proxy.local").proxy_url().unwrap(), "socks5h://proxy.local:1080");
        assert_eq!(proxy("http", "[::1]").proxy_url().unwrap(), "http://[::1]:1080/");
        let with_login = AppSettings {
            proxy_username: "me".into(),
            proxy_password: "p@ss:w/rd".into(),
            ..proxy("http", "proxy.local")
        };
        assert_eq!(with_login.proxy_url().unwrap(), "http://me:p%40ss%3Aw%2Frd@proxy.local:1080/");
        for (proxy_type, host) in
            [("ftp", "proxy.local"), ("http", ""), ("socks5", ""), ("http", "http://proxy.local"), ("http", "a b")]
        {
            assert!(proxy(proxy_type, host).proxy_url().is_err(), "{proxy_type} {host}");
        }
    }
}
