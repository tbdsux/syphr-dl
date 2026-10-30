//! Everything `SyphrApp` draws: title bar, sidebar, toolbar, details card, bulk bar and status bar.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    kbd::Kbd,
    popover::Popover,
    progress::Progress,
    sidebar::{Sidebar, SidebarGroup, SidebarItem as _, SidebarMenu, SidebarMenuItem},
    status_bar::StatusBar,
    table::DataTable,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::download::{Download, Kind, Status, format_bytes};
use crate::ui::downloads_table::{
    DownloadsTable, eta_text, fragment_strip, kind_icon, next_step, progress_color, size_text, speed_text, status_tag,
};
use crate::ui::settings::AppSettings;
use crate::{
    ClearSelection, CloseWindow, CopyLinks, FocusSearch, NewDownload, OpenSettings, Remove, Retry, SelectAll,
    TogglePause,
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
        let (on_change, on_escape) = (add.clone(), add.clone());
        let popover = Popover::new("add-panel")
            .anchor(Anchor::TopCenter)
            .open(open)
            .on_open_change(move |open, window, cx| {
                // Clicks on the field itself also arrive here; keep the panel up while it has focus.
                let field_focused = on_change.read(cx).url().read(cx).focus_handle(cx).is_focused(window);
                if *open || !field_focused {
                    on_change.update(cx, |add, cx| add.set_open(*open, cx));
                }
            })
            .trigger(PanelAnchor::new(field))
            .content(move |_, _, _| add.clone());

        TitleBar::new().on_close_window(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))).child(
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
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_5()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().primary)
                                    .child(
                                        Icon::new(IconName::ArrowDown)
                                            .xsmall()
                                            .text_color(cx.theme().primary_foreground),
                                    ),
                            )
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
                            .child(popover),
                    ),
                )
                .child(div().w(side)),
        )
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |label: &'static str, icon: Icon, filter: Filter, cx: &mut Context<Self>| {
            let count = self.downloads.iter().filter(|d| filter.matches(d)).count();
            SidebarMenuItem::new(label)
                .icon(icon)
                .active(self.filter == filter)
                .suffix(move |_, cx| {
                    // Tabular digits so counts line up down the column.
                    div()
                        .text_xs()
                        .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string())
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter = filter;
                    this.sync_table(cx);
                }))
        };
        Sidebar::new("nav")
            .w(rems(16.))
            .child(SidebarGroup::new("Downloads").child(SidebarMenu::new().children([
                item("All", Icon::new(IconName::Inbox), Filter::All, cx),
                item("Active", Icon::new(IconName::ArrowDown), Filter::Active, cx),
                item("Completed", Icon::new(IconName::CircleCheck), Filter::Completed, cx),
                item("Failed", Icon::new(IconName::CircleX), Filter::Failed, cx),
            ])))
            .child(SidebarGroup::new("Categories").child(SidebarMenu::new().children([
                item("Video", Icon::new(Lucide::Film), Filter::Kind(Kind::Video), cx),
                item("Music", Icon::new(Lucide::Music), Filter::Kind(Kind::Music), cx),
                item("Documents", Icon::new(IconName::FileText), Filter::Kind(Kind::Document), cx),
                item("Programs", Icon::new(Lucide::Package), Filter::Kind(Kind::Program), cx),
                item("Archives", Icon::new(Lucide::FileArchive), Filter::Kind(Kind::Archive), cx),
            ])))
            // A real menu item, so it lines up with the items above.
            .footer(
                SidebarMenu::new()
                    .w_full()
                    .children(AppSettings::get(cx).show_quick_download.then(|| {
                        SidebarMenuItem::new("Quick download").icon(Icon::new(Lucide::ClipboardPaste)).on_click(
                            cx.listener(|this, _, window, cx| {
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
                            }),
                        )
                    }))
                    .child(
                        SidebarMenuItem::new("Settings")
                            .icon(IconName::Settings)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
                    )
                    .render("footer-menu", window, cx),
            )
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
            .fold(summary, |s, (n, what)| format!("{s} · {n} {what}"));
        h_flex()
            .gap_1()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
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
                    .on_click(cx.listener(|this, _, _, cx| this.set_all(&[Status::Paused], Status::Downloading, cx))),
            )
            .child(Button::new("pause-all").small().ghost().icon(IconName::Pause).label("Pause all").on_click(
                cx.listener(|this, _, _, cx| this.set_all(&[Status::Downloading, Status::Queued], Status::Paused, cx)),
            ))
            .child(Input::new(&self.search).small().prefix(IconName::Search).cleanable(true).ml_2().w(rems(17.)))
    }

    fn render_detail(&self, d: &Download, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, surface) = (theme.muted_foreground, theme.border, theme.secondary);
        let (label, icon, next) = next_step(d.status);
        let id = d.id;
        let url = d.url.clone();
        let stat = |title: &'static str, value: AnyElement| {
            v_flex().gap_0p5().min_w_0().child(div().text_xs().text_color(muted).child(title)).child(value)
        };
        let text = |s: String| div().truncate().font_weight(FontWeight::MEDIUM).child(s).into_any_element();

        v_flex()
            .m_3()
            .p_4()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(border)
            .bg(theme.background)
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
                            .bg(surface)
                            .child(kind_icon(d.kind).text_color(muted)),
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
                            .child(
                                Button::new("detail-step").small().outline().icon(icon).label(label).on_click(
                                    cx.listener(move |this, _, window, cx| this.run_step(id, next, window, cx)),
                                ),
                            )
                            .child(
                                Button::new("detail-copy")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .tooltip("Copy link")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()))
                                    }),
                            )
                            .child(
                                Button::new("detail-close")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip("Close details")
                                    .on_click(cx.listener(|this, _, _, cx| this.clear_checked(cx))),
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
                        .child(div().text_xs().text_color(muted).child(format!("{} connections", d.fragments.len())))
                        .child(fragment_strip(d, rems(0.5), cx)),
                )
            })
            .child(
                div()
                    .grid()
                    .grid_cols(6)
                    .gap_4()
                    .child(stat("Status", status_tag(d.status).xsmall().into_any_element()))
                    .child(stat("Downloaded", text(size_text(d))))
                    .child(stat("Speed", text(speed_text(d))))
                    .child(stat("Time left", text(eta_text(d))))
                    .child(stat(
                        "Format",
                        text(d.format.as_ref().map(|f| f.to_string()).unwrap_or_else(|| "Original file".into())),
                    ))
                    .child(stat("Save to", text(d.folder.to_string()))),
            )
    }

    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let pausable = self.checked_with(&[Status::Downloading, Status::Queued]);
        let resumable = self.checked_with(&[Status::Paused]);
        let failed = self.checked_with(&[Status::Failed]);
        let count_label = |verb: &str, n: usize| if n > 0 { format!("{verb} {n}") } else { verb.to_string() };
        h_flex()
            .m_3()
            .px_3()
            .py_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.downloads.iter().filter(|d| d.status == Status::Downloading);
        let (count, speed) = active.fold((0, 0), |(n, s), d| (n + 1, s + d.bytes_per_sec));
        let queued = self.downloads.iter().filter(|d| d.status == Status::Queued).count();
        StatusBar::new().left(div().text_xs().child(format!("{count} downloading · {queued} queued"))).right(
            h_flex()
                .gap_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::ArrowDown).xsmall())
                .child(format!("{}/s", format_bytes(speed))),
        )
    }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.selected_download().cloned();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &NewDownload, window, cx| {
                let auto_paste = AppSettings::get(cx).auto_paste;
                this.add.update(cx, |add, cx| {
                    if auto_paste && add.url().read(cx).value().is_empty() {
                        add.paste_copied_link(window, cx);
                    }
                    add.focus(window, cx);
                    add.set_open(true, cx);
                })
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
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(
                h_flex().flex_1().min_h_0().items_stretch().child(self.render_sidebar(window, cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(self.render_toolbar(cx))
                        .when_some(detail, |this, d| this.child(self.render_detail(&d, cx)))
                        .when(self.checked.len() > 1, |this| this.child(self.render_bulk_bar(cx)))
                        .child({
                            let table = self.table.clone();
                            div().flex_1().min_h_0().relative().child(DataTable::new(&self.table).small()).on_prepaint(
                                move |bounds, window, cx| {
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
                                },
                            )
                        }),
                ),
            )
            .child(self.render_status_bar(cx))
    }
}
