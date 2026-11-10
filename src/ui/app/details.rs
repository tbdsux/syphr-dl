//! The details window for one download, opened by double-clicking its row.

use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    chart::AreaChart,
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState},
    progress::Progress,
    scroll::ScrollableElement as _,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::SyphrApp;
use crate::download::{Download, SPEED_SAMPLES, Source, Status, format_bytes, format_duration};
use crate::engine::{self, Access};
use crate::ui::downloads_table::{
    eta_text, fragment_strip, kind_color, kind_icon, next_step, progress_color, size_text, speed_text, status_label,
};
use crate::ui::settings::AppSettings;

impl SyphrApp {
    /// One window per download; a second double-click brings the open one forward.
    pub fn open_details(&mut self, id: u64, cx: &mut Context<Self>) {
        self.open_details_on(id, LOG_TAB, cx);
    }

    /// Opens the details window on its Options tab, where the address can be replaced.
    pub fn open_refresh_address(&mut self, id: u64, cx: &mut Context<Self>) {
        self.open_details_on(id, OPTIONS_TAB, cx);
    }

    fn open_details_on(&mut self, id: u64, tab: usize, cx: &mut Context<Self>) {
        if let Some((handle, view)) = self.details.get(&id)
            && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
        {
            if tab != LOG_TAB {
                view.update(cx, |view, cx| {
                    view.tab = tab;
                    cx.notify();
                })
                .ok();
            }
            return;
        }
        let app = cx.entity();
        // The new window renders right away and reads the app, which is still being updated here.
        cx.defer(move |cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(640.), px(600.)), cx)),
                window_min_size: Some(size(px(480.), px(420.))),
                ..TitleBar::window_options()
            };
            let view = app.clone();
            if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| DownloadDetails::new(view, id, tab, window, cx))
            }) {
                app.update(cx, |this, _| this.details.insert(id, (handle, view.downgrade())));
            }
        });
    }

    /// Changes one download and saves the list.
    pub fn edit_download(&mut self, id: u64, edit: impl FnOnce(&mut Download), cx: &mut Context<Self>) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
            edit(d);
            self.sync_table(cx);
        }
    }

    /// Points a paused or failed direct download at a new link for the same file, so it resumes from its parts.
    /// The link must serve the same size with byte ranges; otherwise nothing changes and the window says why.
    pub fn refresh_address(&mut self, id: u64, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.downloads.iter().find(|d| d.id == id) else { return };
        let (total, access, probe) = (d.total_bytes, d.access.clone(), url.clone());
        cx.spawn_in(window, async move |this, cx| {
            let info = match engine::RUNTIME.spawn(async move { engine::inspect(&probe, &access).await }).await {
                Ok(result) => result,
                Err(e) => Err(e.to_string()),
            };
            let outcome = match info {
                Err(e) => Err(format!("Couldn't check the new address: {e}")),
                Ok(info) if !info.ranged => {
                    Err("The new address doesn't support resuming, so the download can't continue from it.".into())
                }
                Ok(info) if total.is_some() && info.total != total => Err(format!(
                    "The new address serves a different file ({} instead of {}).",
                    info.total.map(format_bytes).unwrap_or_else(|| "unknown size".into()),
                    total.map(format_bytes).unwrap_or_default(),
                )),
                Ok(info) => Ok(info.summary),
            };
            this.update_in(cx, |this, window, cx| match outcome {
                Ok(summary) => {
                    this.store.log(id, &format!("Address changed to {url} {summary}"));
                    this.edit_download(id, |d| d.url = url.into(), cx);
                    window.push_notification("Address updated. Resume to continue the download.", cx);
                }
                Err(message) => window.push_notification(message, cx),
            })
            .ok();
        })
        .detach();
    }
}

const LOG_TAB: usize = 0;
const OPTIONS_TAB: usize = 1;

pub struct DownloadDetails {
    app: WeakEntity<SyphrApp>,
    id: u64,
    tab: usize,
    /// A replacement link; see `SyphrApp::refresh_address`.
    address: Entity<InputState>,
    /// KB/s; empty uses Settings' limit.
    limit: Entity<InputState>,
    /// Login and header fields, each with its label.
    access: Vec<(&'static str, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}

type AccessField = (&'static str, fn(&Access) -> &String, fn(&mut Access, String));

const ACCESS_FIELDS: [AccessField; 5] = [
    ("Username", |a| &a.username, |a, v| a.username = v),
    ("Password", |a| &a.password, |a, v| a.password = v),
    ("Referer", |a| &a.referer, |a, v| a.referer = v),
    ("Cookie", |a| &a.cookie, |a, v| a.cookie = v),
    ("User agent", |a| &a.user_agent, |a, v| a.user_agent = v),
];

impl DownloadDetails {
    fn new(app: Entity<SyphrApp>, id: u64, tab: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let d = app.read(cx).downloads.iter().find(|d| d.id == id).cloned();
        let url = d.as_ref().map(|d| d.url.clone()).unwrap_or_default();
        let address = cx.new(|cx| InputState::new(window, cx).default_value(url));
        let limit = d.as_ref().and_then(|d| d.limit_kbps).map(|l| l.to_string()).unwrap_or_default();
        let limit = cx.new(|cx| InputState::new(window, cx).placeholder("Default").default_value(limit));
        let mut subscriptions = vec![
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.observe(&address, |_, _, cx| cx.notify()),
            cx.subscribe(&limit, |this, input, event: &InputEvent, cx| {
                if let (InputEvent::Change, Some(limit)) = (event, parse_limit(&input.read(cx).value())) {
                    let id = this.id;
                    this.app.update(cx, |app, cx| app.edit_download(id, |d| d.limit_kbps = limit, cx)).ok();
                }
            }),
        ];
        let saved = d.map(|d| d.access).unwrap_or_default();
        let access = ACCESS_FIELDS
            .iter()
            .map(|&(label, get, set)| {
                let value = get(&saved).clone();
                let input = cx.new(|cx| {
                    InputState::new(window, cx).masked(label == "Password").placeholder("None").default_value(value)
                });
                subscriptions.push(cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let (id, value) = (this.id, input.read(cx).value().to_string());
                        this.app.update(cx, |app, cx| app.edit_download(id, |d| set(&mut d.access, value), cx)).ok();
                    }
                }));
                (label, input)
            })
            .collect();
        Self { app: app.downgrade(), id, tab, address, limit, access, _subscriptions: subscriptions }
    }
}

/// None when the text isn't a limit; Some(None) when empty, which means Settings' limit.
fn parse_limit(text: &str) -> Option<Option<u64>> {
    match text.trim() {
        "" => Some(None),
        text => text.parse().ok().map(Some),
    }
}

impl Render for DownloadDetails {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = cx.theme().sidebar;
        let found = self.app.upgrade().and_then(|app| {
            let app = app.read(cx);
            let d = app.downloads.iter().find(|d| d.id == self.id)?.clone();
            Some((d, app.store.log_for(self.id).unwrap_or_default()))
        });
        let title = found.as_ref().map_or_else(|| "Download".into(), |(d, _)| d.name.clone());
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().bg(chrome).border_color(chrome).child(div().truncate().child(title)))
            .child(match found {
                Some((d, lines)) => self.render_body(&d, lines, cx).into_any_element(),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("This download was removed from the list.")
                    .into_any_element(),
            })
    }
}

impl DownloadDetails {
    fn render_body(
        &self,
        d: &crate::download::Download,
        lines: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, surface) = (theme.muted_foreground, theme.secondary);
        let (radius, mono) = (theme.radius, theme.mono_font_family.clone());
        let (label, icon, next) = next_step(d.status);
        let kind_tint = kind_color(d.kind, cx);
        let id = d.id;
        let url = d.url.clone();
        let app = self.app.clone();
        let stat = |title: &'static str, value: AnyElement| {
            v_flex().gap_0p5().min_w_0().child(div().text_xs().text_color(muted).child(title)).child(value)
        };
        let text = |s: String| div().truncate().font_weight(FontWeight::MEDIUM).child(s).into_any_element();

        v_flex()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size_9()
                            .rounded(theme.radius)
                            .bg(kind_tint.opacity(0.16))
                            .child(kind_icon(d.kind).text_color(kind_tint)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(d.name.clone()))
                            .child(div().truncate().text_xs().text_color(muted).child(d.url.clone())),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Button::new("detail-step").small().outline().icon(icon).label(label).on_click(
                                move |_, window, cx| {
                                    app.update(cx, |this, cx| this.run_step(id, next, window, cx)).ok();
                                },
                            ))
                            .child(
                                Button::new("detail-copy")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .tooltip("Copy link")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()))
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Progress::new("detail-progress")
                            .value(d.progress())
                            .color(progress_color(d.status, cx))
                            .accessibility_label(format!("{} progress", d.name))
                            .flex_1(),
                    )
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(format!("{:.1}%", d.progress()))),
            )
            .when(!d.fragments.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(format!("{} parts", d.fragments.len())))
                        .child(fragment_strip(d, rems(0.5), cx)),
                )
            })
            .when(!d.speed_history.is_empty(), |this| {
                let color = progress_color(d.status, cx);
                let newest = d.speed_history.len() - 1;
                this.child(
                    div().h(rems(5.)).child(
                        AreaChart::new(d.speed_history.iter().map(|&speed| speed as f64).enumerate())
                            .x(|(i, _)| i.to_string())
                            .y(|(_, speed)| *speed)
                            .name("Speed")
                            .linear()
                            .stroke(color)
                            .fill(color.opacity(0.2))
                            .point_count(SPEED_SAMPLES)
                            .x_axis(false)
                            .y_axis(true)
                            .y_tick_count(2)
                            .y_tick_format(|speed| format!("{}/s", format_bytes(speed as u64)))
                            .tooltip_title(move |(i, _)| {
                                format!("{} ago", format_duration(((newest - i) / 2) as u64)).into()
                            })
                            .tooltip_value(|_, _, speed| format!("{}/s", format_bytes(speed as u64)).into())
                            .appear(false),
                    ),
                )
            })
            .child(
                div()
                    .grid()
                    .grid_cols(3)
                    .gap_4()
                    .child(stat("Status", status_label(d.status, cx).into_any_element()))
                    .child(stat("Downloaded", text(size_text(d))))
                    .child(stat("Speed", text(speed_text(d))))
                    .child(stat("Time left", text(eta_text(d))))
                    .child(stat(
                        "Format",
                        text(d.format.as_ref().map(|f| f.to_string()).unwrap_or_else(|| "Original file".into())),
                    ))
                    .child(stat("Save to", text(d.folder.to_string()))),
            )
            .child(
                TabBar::new("detail-tabs")
                    .underline()
                    .small()
                    .selected_index(self.tab)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = *ix;
                        cx.notify();
                    }))
                    .child(Tab::new().label("Log"))
                    .child(Tab::new().label("Options")),
            )
            .map(|this| {
                if self.tab != LOG_TAB {
                    return this.child(self.render_options(d, cx));
                }
                // Newest first, so a running download's latest line is in view without scrolling.
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h(rems(6.))
                        .overflow_y_scrollbar()
                        .gap_0p5()
                        .p_2()
                        .rounded(radius)
                        .bg(surface)
                        .font_family(mono)
                        .text_xs()
                        .when(lines.is_empty(), |this| this.text_color(muted).child("Nothing logged yet"))
                        .children(lines.into_iter().rev().map(|(at, message)| {
                            h_flex()
                                .gap_3()
                                .items_start()
                                .child(div().flex_none().text_color(muted).child(at))
                                .child(div().flex_1().min_w_0().child(message))
                        })),
                )
            })
    }

    fn render_options(&self, d: &Download, cx: &mut Context<Self>) -> impl IntoElement {
        let address = self.address.read(cx).value().trim().to_string();
        let can_refresh = d.source == Source::Direct && matches!(d.status, Status::Paused | Status::Failed);
        let (app, id, muted) = (self.app.clone(), self.id, cx.theme().muted_foreground);
        let default = match AppSettings::get(cx).speed_limit_kbps as u64 {
            0 => "no limit".to_string(),
            kbps => format!("{kbps} KB/s"),
        };
        let limit_help = match parse_limit(&self.limit.read(cx).value()) {
            Some(_) => format!("Empty uses the default from Settings ({default}). 0 means no limit."),
            None => "Enter a whole number of KB/s.".into(),
        };
        v_flex().flex_1().min_h(rems(6.)).overflow_y_scrollbar().child(
            Form::new()
                .when(d.source == Source::Direct, |this| {
                    this.child(
                        Field::new()
                            .label("Address")
                            .description(if can_refresh {
                                "Paste a fresh link to the same file, such as a new expiring link, then refresh."
                            } else {
                                "Pause the download to change its address."
                            })
                            .child(
                                h_flex().gap_2().child(Input::new(&self.address).small().flex_1()).child(
                                    Button::new("refresh-address")
                                        .small()
                                        .outline()
                                        .label("Refresh")
                                        .disabled(!can_refresh || address.is_empty() || address == *d.url)
                                        .on_click(move |_, window, cx| {
                                            let url = address.clone();
                                            app.update(cx, |this, cx| this.refresh_address(id, url, window, cx)).ok();
                                        }),
                                ),
                            ),
                    )
                })
                .child(
                    Field::new()
                        .label("Speed limit (KB/s)")
                        .description(limit_help)
                        .child(Input::new(&self.limit).small()),
                )
                .children(self.access.iter().map(|(label, input)| {
                    let field = Input::new(input).small();
                    let field = if *label == "Password" { field.mask_toggle() } else { field };
                    Field::new().label(*label).child(field)
                }))
                .child(Field::new().child(div().text_xs().text_color(muted).child(
                    "The login goes out as HTTP Basic, or to yt-dlp. Empty fields use the link's own login or a \
                     saved one from Settings → Logins. Changes apply when the download next starts.",
                ))),
        )
    }
}
