use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState},
    radio::{Radio, RadioGroup},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::download::{Download, Kind, Source, Status, format_bytes, format_duration};
use crate::engine::ytdlp::{self, VideoInfo};
use crate::engine::{self, LinkInfo};
use crate::ui::settings::AppSettings;

enum Lookup {
    /// Nothing that looks like a link yet.
    NotNeeded,
    Loading,
    Ready(VideoInfo),
    File(LinkInfo),
    Failed(SharedString),
}

pub enum AddEvent {
    Submit,
}

/// The title-bar link field's state; it renders the panel that drops down below the field.
pub struct AddDownload {
    url: Entity<InputState>,
    folder: Entity<InputState>,
    name: Entity<InputState>,
    /// The guess last put in `name`; a different value means the user typed their own.
    auto_name: String,
    open: bool,
    lookup: Lookup,
    choice: usize,
    _lookup_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AddDownload {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("Paste a link or YouTube URL to download"));
        let dir = AppSettings::get(cx).download_dir.clone();
        let folder = cx.new(|cx| InputState::new(window, cx).default_value(dir));
        let name = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe(&url, |this, _, event, cx| match event {
            InputEvent::Change => this.url_changed(cx),
            InputEvent::PressEnter { .. } => cx.emit(AddEvent::Submit),
            _ => {}
        });
        Self {
            url,
            folder,
            name,
            auto_name: String::new(),
            open: false,
            lookup: Lookup::NotNeeded,
            choice: 0,
            _lookup_task: None,
            _subscriptions: vec![subscription],
        }
    }

    pub fn url(&self) -> &Entity<InputState> {
        &self.url
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens only when there is a link to act on.
    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let open = open && !self.url_text(cx).is_empty();
        if self.open != open {
            self.open = open;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.url.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Clears the field after a download was added or the user cancelled.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.url.update(cx, |state, cx| state.set_value("", window, cx));
        let dir = AppSettings::get(cx).download_dir.clone();
        self.folder.update(cx, |state, cx| state.set_value(dir, window, cx));
        self.name.update(cx, |state, cx| state.set_value("", window, cx));
        self.auto_name.clear();
        self.lookup = Lookup::NotNeeded;
        self._lookup_task = None;
        self.open = false;
        cx.notify();
    }

    /// Fills the field with the copied link and opens the panel; false when the clipboard holds no link.
    pub fn paste_copied_link(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(link) = cx.read_from_clipboard().and_then(|item| item.text()).map(|t| t.trim().to_string()) else {
            return false;
        };
        if !is_web_link(&link) {
            return false;
        }
        self.url.update(cx, |state, cx| state.set_value(link, window, cx));
        self.url_changed(cx);
        true
    }

    fn url_text(&self, cx: &App) -> String {
        self.url.read(cx).value().trim().to_string()
    }

    fn url_changed(&mut self, cx: &mut Context<Self>) {
        let url = self.url_text(cx);
        // Replacing the task drops (cancels) any lookup for the previous text.
        self._lookup_task = None;
        self.open = !url.is_empty();
        if !is_web_link(&url) {
            self.lookup = Lookup::NotNeeded;
            cx.notify();
            return;
        }
        self.lookup = Lookup::Loading;
        cx.notify();
        let bin = ytdlp::handles(&url).then(|| crate::tools::resolve(&AppSettings::get(cx).ytdlp_path));
        self._lookup_task = Some(cx.spawn(async move |this, cx| {
            // Let typing or a paste settle before starting a process or request.
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let result = match bin {
                Some(bin) => cx.background_spawn(async move { ytdlp::probe(&bin, &url) }).await.map(Lookup::Ready),
                // reqwest needs the tokio runtime; the handle is awaited from here.
                None => match engine::RUNTIME.spawn(async move { engine::inspect(&url).await }).await {
                    Ok(result) => result.map(Lookup::File),
                    Err(e) => Err(e.to_string()),
                },
            };
            this.update(cx, |this, cx| {
                this.choice = 0;
                this.lookup = result.unwrap_or_else(|message| Lookup::Failed(message.into()));
                cx.notify();
            })
            .ok();
        }));
    }

    /// The download to add, or an explanation of what's missing.
    pub fn request(&self, id: u64, cx: &App) -> Result<Download, &'static str> {
        let url = self.url_text(cx);
        if !is_web_link(&url) {
            return Err("Enter a link that starts with http:// or https://");
        }
        let folder: SharedString = self.folder.read(cx).value().trim().to_string().into();
        let typed = self.name.read(cx).value().trim().to_string();
        let file_name = (!typed.is_empty() && typed != self.auto_name).then_some(typed);
        let base = Download {
            id,
            name: "".into(),
            url: url.clone().into(),
            kind: kind_for(&url),
            source: Source::Direct,
            status: Status::Queued,
            total_bytes: None,
            received_bytes: 0,
            bytes_per_sec: 0,
            folder,
            format: None,
            selector: None,
            file_name: file_name.clone(),
            path: None,
            fragments: Vec::new(),
        };
        match &self.lookup {
            Lookup::NotNeeded => Err("Enter a link that starts with http:// or https://"),
            Lookup::Loading => Err("Wait for the link to be checked."),
            Lookup::Failed(_) => Err("This link couldn't be checked. Fix the link and try again."),
            Lookup::File(info) => Ok(Download {
                name: file_name.unwrap_or_else(|| info.name.clone()).into(),
                kind: kind_for(&info.name),
                total_bytes: info.total,
                ..base
            }),
            Lookup::Ready(info) => {
                let option = &info.options[self.choice.min(info.options.len() - 1)];
                Ok(Download {
                    name: file_name.map(Into::into).unwrap_or_else(|| info.title.clone()),
                    kind: if option.audio_only { Kind::Music } else { Kind::Video },
                    source: Source::YtDlp,
                    total_bytes: option.size_bytes,
                    format: Some(option.label.clone()),
                    selector: Some(option.selector.clone()),
                    ..base
                })
            }
        }
    }

    fn guessed_name(&self) -> String {
        match &self.lookup {
            Lookup::Ready(info) => info.title.to_string(),
            Lookup::File(info) => info.name.clone(),
            _ => String::new(),
        }
    }

    fn render_lookup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let muted = cx.theme().muted_foreground;
        Some(match &self.lookup {
            Lookup::NotNeeded => return None,
            Lookup::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(if ytdlp::handles(&self.url_text(cx)) {
                    "Looking up available formats with yt-dlp…"
                } else {
                    "Checking the link…"
                })
                .into_any_element(),
            Lookup::Failed(message) => h_flex()
                .gap_2()
                .items_start()
                .text_sm()
                .child(Icon::new(IconName::CircleX).small().text_color(cx.theme().danger))
                .child(div().flex_1().child(message.clone()))
                .when(ytdlp::handles(&self.url_text(cx)), |this| {
                    this.children(crate::tools::install_button(cx).map(|b| b.small()))
                })
                .child(
                    Button::new("retry")
                        .small()
                        .outline()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.url_changed(cx))),
                )
                .into_any_element(),
            Lookup::Ready(info) => {
                let meta = [info.uploader.as_ref().map(|u| u.to_string()), info.duration_secs.map(format_duration)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().font_weight(FontWeight::MEDIUM).line_clamp(2).child(info.title.clone()))
                            .when(!meta.is_empty(), |this| this.child(div().text_xs().text_color(muted).child(meta))),
                    )
                    .child(
                        RadioGroup::vertical("format")
                            .selected_index(Some(self.choice))
                            // By index: "Best quality" repeats the top option's selector, so selectors aren't unique ids.
                            .children(
                                info.options.iter().enumerate().map(|(ix, o)| {
                                    Radio::new(("format", ix)).label(format!("{} — {}", o.label, o.detail))
                                }),
                            )
                            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                this.choice = *ix;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            Lookup::File(info) => {
                let ext = std::path::Path::new(&info.name).extension().map(|e| e.to_string_lossy().to_uppercase());
                let size = info.total.map(format_bytes).unwrap_or_else(|| "Size unknown".into());
                let mime = info.content_type.as_deref().and_then(|t| t.split(';').next()).map(|t| t.trim().to_string());
                let meta = [ext, Some(size), mime].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                v_flex()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::MEDIUM).line_clamp(2).child(info.name.clone()))
                    .child(div().text_xs().text_color(muted).child(meta))
                    .when(!info.ranged, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Downloads in one part; can't resume after a pause."),
                        )
                    })
                    .into_any_element()
            }
        })
    }
}

fn is_web_link(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() > "https://".len()
}

pub fn kind_for(url: &str) -> Kind {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let ext = path.rsplit('.').next().unwrap_or_default().to_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "webm" | "mov" | "avi" => Kind::Video,
        "mp3" | "m4a" | "flac" | "wav" | "ogg" => Kind::Music,
        "zip" | "rar" | "7z" | "tar" | "gz" => Kind::Archive,
        "exe" | "msi" | "dmg" | "pkg" | "deb" | "iso" | "appimage" => Kind::Program,
        _ => Kind::Document,
    }
}

impl EventEmitter<AddEvent> for AddDownload {}

impl Render for AddDownload {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the name field showing the latest guess until the user types over it.
        let guess = self.guessed_name();
        if guess != self.auto_name {
            if self.name.read(cx).value().trim() == self.auto_name {
                self.name.update(cx, |state, cx| state.set_value(guess.clone(), window, cx));
            }
            self.auto_name = guess;
        }
        let url = self.url_text(cx);
        // Name, folder and Start appear only once the link checked out.
        let ready = matches!(self.lookup, Lookup::Ready(_) | Lookup::File(_));
        let label = if matches!(self.lookup, Lookup::File(_)) { "File" } else { "Format" };
        let hint = (!is_web_link(&url)).then_some("Links start with http:// or https://");
        let lookup = self.render_lookup(cx);
        v_flex()
            .w(rems(34.))
            .gap_3()
            .when_some(hint, |this, hint| {
                this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(hint))
            })
            .child(
                Form::new().when_some(lookup, |this, lookup| this.child(Field::new().label(label).child(lookup))).when(
                    ready,
                    |this| {
                        this.child(
                            Field::new()
                                .label("File name")
                                .child(Input::new(&self.name).small().prefix(IconName::File)),
                        )
                        .child(
                            Field::new()
                                .label("Save to")
                                .child(Input::new(&self.folder).small().prefix(IconName::Folder)),
                        )
                    },
                ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-add")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    )
                    .child(
                        Button::new("start")
                            .small()
                            .primary()
                            .label("Start download")
                            .disabled(!ready)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(AddEvent::Submit))),
                    ),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.set_open(false, cx);
                    window.blur(cx);
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, is_web_link, kind_for};

    #[test]
    fn link_checks() {
        assert!(is_web_link("https://a.com/x.zip"));
        assert!(!is_web_link("https://"));
        assert!(!is_web_link("ftp://a.com/x"));
        assert_eq!(kind_for("https://a.com/setup.dmg?token=1"), Kind::Program);
        assert_eq!(kind_for("https://a.com/song.MP3"), Kind::Music);
    }
}
