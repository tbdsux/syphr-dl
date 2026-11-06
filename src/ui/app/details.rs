//! The details window for one download, opened by double-clicking its row.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    chart::AreaChart,
    h_flex,
    progress::Progress,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::SyphrApp;
use crate::download::{SPEED_SAMPLES, format_bytes, format_duration};
use crate::ui::downloads_table::{
    eta_text, fragment_strip, kind_color, kind_icon, next_step, progress_color, size_text, speed_text, status_label,
};

impl SyphrApp {
    /// One window per download; a second double-click brings the open one forward.
    pub fn open_details(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(handle) = self.details.get(&id)
            && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
        {
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
            if let Ok((handle, _)) =
                gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| DownloadDetails::new(view, id, cx)))
            {
                app.update(cx, |this, _| this.details.insert(id, handle));
            }
        });
    }
}

pub struct DownloadDetails {
    app: WeakEntity<SyphrApp>,
    id: u64,
    show_log: bool,
    _observe: Subscription,
}

impl DownloadDetails {
    fn new(app: Entity<SyphrApp>, id: u64, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&app, |_, _, cx| cx.notify());
        Self { app: app.downgrade(), id, show_log: true, _observe: observe }
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
                                Button::new("detail-log")
                                    .small()
                                    .ghost()
                                    .icon(Icon::new(Lucide::ScrollText))
                                    .selected(self.show_log)
                                    .tooltip(if self.show_log { "Hide log" } else { "Show log" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_log = !this.show_log;
                                        cx.notify();
                                    })),
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
            .when(self.show_log, |this| {
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
}
