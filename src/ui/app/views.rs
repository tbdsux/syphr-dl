//! Everything `SyphrApp` draws: title bar, sidebar, toolbar, bulk bar and status bar.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, Size, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    kbd::Kbd,
    popover::Popover,
    scroll::ScrollableElement as _,
    table::DataTable,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::download::{Kind, Status, format_bytes};
use crate::ui::batch::BatchKind;
use crate::ui::downloads_table::{DownloadsTable, kind_color, kind_icon};
use crate::ui::settings::AppSettings;
use crate::{
    BatchDownload, ClearSelection, CloseWindow, CopyLinks, FocusSearch, NewDownload, OpenSettings, Remove, Retry,
    SelectAll, TogglePause,
};

use super::{Filter, SyphrApp};

impl SyphrApp {
    /// The link field sits centered in the title bar; its panel drops down with formats and the save folder.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let add = self.add.clone();
        let url = add.read(cx).url().clone();
        let open = add.read(cx).is_open();
        let empty = url.read(cx).value().is_empty();
        let side = rems(9.25);
        let field = Input::new(&url)
            .small()
            .prefix(IconName::Globe)
            .when(empty, |this| {
                this.suffix(Kbd::new(Keystroke::parse(secondary!("n")).expect("valid keystroke")).appearance(false))
            })
            .cleanable(true)
            .w(rems(34.));
        let (on_change, on_escape, on_prepaint) = (add.clone(), add.clone(), add.clone());
        let popover = Popover::new("add-panel")
            .anchor(Anchor::TopCenter)
            .open(open)
            // An opening popover takes focus unless told otherwise; typing has to stay in the field.
            .track_focus(&url.read(cx).focus_handle(cx))
            .on_open_change(move |open, window, cx| {
                // Clicks on the field itself also arrive here; keep the panel up for those.
                let on_field = on_change.read(cx).field_bounds.contains(&window.mouse_position());
                if *open || !on_field {
                    on_change.update(cx, |add, cx| add.set_open(*open, cx));
                }
            })
            .trigger(PanelAnchor::new(field))
            .content(move |_, _, _| add.clone());

        let chrome = cx.theme().sidebar;
        // Title bar, sidebar and status bar read as one frame around the content panel.
        let title_bar = TitleBar::new().bg(chrome).border_color(chrome);
        title_bar.on_close_window(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))).child(
            h_flex()
                .w_full()
                .pr_2()
                // macOS shows the menus in the system menu bar; elsewhere they live in the window.
                .map(|this| match &self.menu_bar {
                    Some(menu_bar) => this.child(div().w(side).child(menu_bar.clone())),
                    None => this.child(
                        h_flex()
                            .w(side)
                            .gap_2()
                            .child(img(crate::LOGO).size_5())
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Syphr")),
                    ),
                })
                .child(
                    h_flex().flex_1().justify_center().child(
                        div()
                            // Keep presses in the field from dragging the window.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" {
                                    on_escape.update(cx, |add, cx| add.set_open(false, cx));
                                    window.blur(cx);
                                }
                            })
                            .on_prepaint(move |bounds, _, cx| {
                                on_prepaint.update(cx, |add, _| add.field_bounds = bounds);
                            })
                            .child(popover),
                    ),
                )
                .child(div().w(side)),
        )
    }

    /// Every filter row is also a bar: its wash shows how much of the list that filter covers.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.downloads.len().max(1) as f32;
        let filter_row =
            |label: &'static str, icon: Icon, filter: Filter, tint: Option<Hsla>, cx: &mut Context<Self>| {
                let count = self.downloads.iter().filter(|d| filter.matches(d)).count();
                let active = self.filter == filter;
                let theme = cx.theme();
                let (muted, foreground) = (theme.muted_foreground, theme.foreground);
                let icon = icon.when_some(tint, |icon, color| icon.text_color(color));
                nav_row(label, icon, tint.map(|color| (count as f32 / total, color)), cx)
                    .text_color(if active || count > 0 { foreground } else { muted })
                    .when(active, |this| this.bg(theme.background))
                    .when(!active, |this| this.hover(|this| this.bg(theme.sidebar_accent)))
                    .aria_selected(active)
                    .child(
                        // Tabular digits so counts line up down the column.
                        div()
                            .text_sm()
                            .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                            .text_color(muted)
                            .when(count > 0, |this| this.child(count.to_string())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = filter;
                        this.sync_table(cx);
                    }))
            };
        let kind_row = |label: &'static str, kind: Kind, cx: &mut Context<Self>| {
            let color = kind_color(kind, cx);
            filter_row(label, kind_icon(kind), Filter::Kind(kind), Some(color), cx)
        };
        let theme = cx.theme();
        let (primary, success, danger) = (theme.primary, theme.success, theme.danger);
        let (muted, hover) = (theme.muted_foreground, theme.sidebar_accent);
        v_flex()
            .id("nav")
            .role(Role::Tree)
            .flex_none()
            .w(rems(15.))
            .h_full()
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(group_title("Downloads", cx))
                            .child(filter_row("All", Icon::new(IconName::Inbox), Filter::All, None, cx))
                            .child(filter_row(
                                "Active",
                                Icon::new(IconName::ArrowDown),
                                Filter::Active,
                                Some(primary),
                                cx,
                            ))
                            .child(filter_row(
                                "Completed",
                                Icon::new(IconName::CircleCheck),
                                Filter::Completed,
                                Some(success),
                                cx,
                            ))
                            .child(filter_row(
                                "Failed",
                                Icon::new(IconName::CircleX),
                                Filter::Failed,
                                Some(danger),
                                cx,
                            )),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(group_title("Categories", cx))
                            .child(kind_row("Video", Kind::Video, cx))
                            .child(kind_row("Music", Kind::Music, cx))
                            .child(kind_row("Documents", Kind::Document, cx))
                            .child(kind_row("Programs", Kind::Program, cx))
                            .child(kind_row("Archives", Kind::Archive, cx)),
                    )
                    .overflow_y_scrollbar(),
            )
            .child(self.render_transfer(cx))
            .child(
                v_flex()
                    .pb_2()
                    .gap_0p5()
                    .children(crate::update::available(cx).map(|release| {
                        let label = crate::update::busy(cx).unwrap_or(format!("Update to {}", release.tag).into());
                        nav_row(label, Icon::new(Lucide::CircleArrowUp), None, cx)
                            .hover(|this| this.bg(hover))
                            .on_click(|_, window, cx| crate::update::install(window.window_handle(), cx))
                    }))
                    .children(AppSettings::get(cx).show_quick_download.then(|| {
                        nav_row("Quick download", Icon::new(Lucide::ClipboardPaste), None, cx)
                            .text_color(muted)
                            .hover(|this| this.bg(hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let pasted = this.add.update(cx, |add, cx| {
                                    let pasted = add.paste_copied_link(window, cx);
                                    if pasted {
                                        add.focus(window, cx);
                                    }
                                    pasted
                                });
                                if !pasted {
                                    window.push_notification(
                                        "Copy a link that starts with http:// or https:// first.",
                                        cx,
                                    );
                                }
                            }))
                    }))
                    .child(
                        nav_row("Settings", Icon::new(IconName::Settings), None, cx)
                            .text_color(muted)
                            .hover(|this| this.bg(hover))
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
                    ),
            )
    }

    /// What is moving right now, at the foot of the sidebar.
    fn render_transfer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.downloads.iter().filter(|d| d.status == Status::Downloading);
        let (count, speed) = active.fold((0, 0), |(n, s), d| (n + 1, s + d.bytes_per_sec));
        let queued = self.downloads.iter().filter(|d| d.status == Status::Queued).count();
        let detail = match (count, queued) {
            (0, 0) => "Nothing downloading".to_string(),
            (n, 0) => format!("{n} downloading"),
            (0, q) => format!("{q} queued"),
            (n, q) => format!("{n} downloading, {q} queued"),
        };
        let theme = cx.theme();
        v_flex()
            .px_5()
            .py_3()
            .when(count > 0, |this| {
                this.child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                        .child(format!("{}/s", format_bytes(speed))),
                )
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(detail))
    }

    /// The current list's name and what it holds, with the commands that act on all of it.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.downloads.iter().filter(|d| self.filter.matches(d));
        let (total, active, done) = shown.fold((0, 0, 0), |(t, a, c), d| {
            (t + 1, a + usize::from(d.status == Status::Downloading), c + usize::from(d.status == Status::Completed))
        });
        let summary = match total {
            0 => "Empty".to_string(),
            1 => "1 download".to_string(),
            n => format!("{n} downloads"),
        };
        let summary = [(active, "downloading"), (done, "completed")]
            .into_iter()
            .filter(|(n, _)| *n > 0 && *n != total)
            .fold(summary, |s, (n, what)| format!("{s}, {n} {what}"));
        h_flex()
            .gap_1()
            .px_4()
            .py_3()
            .child(
                v_flex()
                    .mr_3()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(self.filter.title()))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(summary)),
            )
            .child(div().flex_1())
            .child(
                Button::new("resume-all")
                    .small()
                    .ghost()
                    .icon(IconName::Play)
                    .label("Resume all")
                    .on_click(cx.listener(|this, _, _, cx| this.resume_all(cx))),
            )
            .child(
                Button::new("pause-all")
                    .small()
                    .ghost()
                    .icon(IconName::Pause)
                    .label("Pause all")
                    .on_click(cx.listener(|this, _, _, cx| this.pause_all(cx))),
            )
            .child(Input::new(&self.search).small().prefix(IconName::Search).cleanable(true).ml_2().w(rems(17.)))
    }

    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let pausable = self.checked_with(&[Status::Downloading, Status::Queued]);
        let resumable = self.checked_with(&[Status::Paused]);
        let failed = self.checked_with(&[Status::Failed]);
        let count_label = |verb: &str, n: usize| if n > 0 { format!("{verb} {n}") } else { verb.to_string() };
        h_flex()
            .px_4()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(Icon::new(Lucide::ListChecks).small().text_color(theme.muted_foreground))
            .child(
                div().pl_1().pr_2().font_weight(FontWeight::MEDIUM).child(format!("{} selected", self.checked.len())),
            )
            .child(
                Button::new("bulk-resume")
                    .small()
                    .ghost()
                    .icon(IconName::Play)
                    .label(count_label("Resume", resumable))
                    .disabled(resumable == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_checked_status(&[Status::Paused], Status::Downloading, cx)
                    })),
            )
            .child(
                Button::new("bulk-pause")
                    .small()
                    .ghost()
                    .icon(IconName::Pause)
                    .label(count_label("Pause", pausable))
                    .disabled(pausable == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_checked_status(&[Status::Downloading, Status::Queued], Status::Paused, cx)
                    })),
            )
            .child(
                Button::new("bulk-retry")
                    .small()
                    .ghost()
                    .icon(IconName::RotateCw)
                    .label(count_label("Retry", failed))
                    .disabled(failed == 0)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_checked_status(&[Status::Failed], Status::Queued, cx)),
                    ),
            )
            .child(
                Button::new("bulk-copy")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .label("Copy links")
                    .on_click(cx.listener(|this, _, window, cx| this.copy_checked_links(window, cx))),
            )
            .child(
                Button::new("bulk-remove")
                    .small()
                    .ghost()
                    .icon(IconName::Delete)
                    .label("Remove…")
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_remove_checked(window, cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("bulk-clear")
                    .small()
                    .ghost()
                    .label("Clear selection")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_checked(cx))),
            )
    }
}

/// Lines up with the icons in the `nav_row`s below it.
fn group_title(title: &'static str, cx: &App) -> Div {
    h_flex().flex_none().h_8().px_5().text_xs().text_color(cx.theme().muted_foreground).child(title)
}

/// A sidebar row over an optional `(share, color)` wash.
fn nav_row(label: impl Into<SharedString>, icon: Icon, wash: Option<(f32, Hsla)>, cx: &App) -> Stateful<Div> {
    let label = label.into();
    let radius = cx.theme().radius;
    h_flex()
        .id(ElementId::Name(label.clone()))
        .role(Role::TreeItem)
        .aria_label(label.clone())
        .relative()
        .flex_none()
        .h_9()
        .ml_2()
        .px_3()
        .gap_2p5()
        .text_sm()
        .rounded(radius)
        .cursor_pointer()
        .children(wash.filter(|(share, _)| *share > 0.).map(|(share, color)| {
            div().absolute().top_0().bottom_0().left_0().w(relative(share)).rounded(radius).bg(color.opacity(0.16))
        }))
        .child(icon.small())
        .child(div().flex_1().min_w_0().truncate().child(label))
}

/// Popover wants a `Selectable` trigger; this lets the link field be one.
#[derive(IntoElement)]
struct PanelAnchor {
    child: AnyElement,
    open: bool,
}

impl PanelAnchor {
    fn new(child: impl IntoElement) -> Self {
        Self { child: child.into_any_element(), open: false }
    }
}

impl Selectable for PanelAnchor {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for PanelAnchor {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.child
    }
}

impl Render for SyphrApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &NewDownload, window, cx| this.new_download(window, cx)))
            .on_action(cx.listener(|this, _: &BatchDownload, window, cx| {
                this.open_batch(BatchKind::Pattern, Vec::new(), None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                let handle = this.search.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePause, _, cx| this.toggle_pause_checked(cx)))
            .on_action(
                cx.listener(|this, _: &Retry, _, cx| this.set_checked_status(&[Status::Failed], Status::Queued, cx)),
            )
            .on_action(cx.listener(|this, _: &CopyLinks, window, cx| {
                if !this.checked.is_empty() {
                    this.copy_checked_links(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Remove, window, cx| {
                if !this.checked.is_empty() {
                    this.confirm_remove_checked(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.check_all(true, cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| this.clear_checked(cx)))
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(
                h_flex().flex_1().min_h_0().items_stretch().child(self.render_sidebar(cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .mx_2()
                        .mb_2()
                        .rounded(cx.theme().radius_lg)
                        .bg(cx.theme().background)
                        .overflow_hidden()
                        .child(self.render_toolbar(cx))
                        .when(self.checked.len() > 1, |this| this.child(self.render_bulk_bar(cx)))
                        .child({
                            let table = self.table.clone();
                            div()
                                .flex_1()
                                .min_h_0()
                                .relative()
                                .child(
                                    DataTable::new(&self.table)
                                        .with_size(Size::Size(cx.theme().font_size * 3.25))
                                        .header_row_height(cx.theme().font_size * 2.25)
                                        .bordered(false),
                                )
                                .on_prepaint(move |bounds, window, cx| {
                                    let rem = window.rem_size();
                                    table.update(cx, |table, cx| {
                                        let d = table.delegate();
                                        if d.width != bounds.size.width || d.rem != rem {
                                            table.delegate_mut().width = bounds.size.width;
                                            table.delegate_mut().rem = rem;
                                            table.refresh(cx);
                                            cx.notify();
                                        } else {
                                            DownloadsTable::follow_drag(table, window, cx);
                                        }
                                    })
                                })
                        }),
                ),
            )
    }
}
