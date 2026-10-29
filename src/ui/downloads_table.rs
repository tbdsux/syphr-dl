use gpui_kit::assets::IconName as Lucide;
use std::collections::BTreeSet;

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
    table::{Column, TableDelegate, TableState},
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::download::{Download, Kind, Source, Status, format_bytes, format_duration};
use crate::ui::app::SyphrApp;

const CHECK: usize = 0;
const NAME: usize = 1;
const PROGRESS: usize = 2;
const SIZE: usize = 3;
const SPEED: usize = 4;
const ETA: usize = 5;
const STATUS: usize = 6;
const ACTIONS: usize = 7;

/// Rows are a filtered snapshot that `SyphrApp` refreshes whenever its list changes.
pub struct DownloadsTable {
    pub rows: Vec<Download>,
    pub checked: BTreeSet<u64>,
    pub empty: (&'static str, &'static str),
    /// Measured table width; Name takes whatever the fixed columns leave.
    pub width: Pixels,
    app: WeakEntity<SyphrApp>,
}

impl DownloadsTable {
    pub fn new(app: WeakEntity<SyphrApp>) -> Self {
        Self { rows: Vec::new(), checked: BTreeSet::new(), empty: ("", ""), width: px(0.), app }
    }
}

pub fn kind_icon(kind: Kind) -> Icon {
    match kind {
        Kind::Video => Icon::new(Lucide::Film),
        Kind::Music => Icon::new(Lucide::Music),
        Kind::Document => Icon::new(IconName::FileText),
        Kind::Program => Icon::new(Lucide::Package),
        Kind::Archive => Icon::new(Lucide::FileArchive),
    }
}

pub fn status_tag(status: Status) -> Tag {
    match status {
        Status::Downloading => Tag::info().child("Downloading"),
        Status::Paused => Tag::secondary().child("Paused"),
        Status::Queued => Tag::secondary().child("Queued"),
        Status::Completed => Tag::success().child("Completed"),
        Status::Missing => Tag::warning().child("Missing"),
        Status::Failed => Tag::danger().child("Failed"),
    }
}

pub fn progress_color(status: Status, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        Status::Completed => theme.success,
        Status::Failed => theme.danger,
        Status::Paused | Status::Queued | Status::Missing => theme.muted_foreground,
        Status::Downloading => theme.primary,
    }
}

/// One segment per parallel part, in file order, so stalled connections stand out.
pub fn fragment_strip(d: &Download, height: Pixels, cx: &App) -> Div {
    let (color, track) = (progress_color(d.status, cx), cx.theme().secondary);
    h_flex().gap_px().children(d.fragments.iter().map(|f| {
        div()
            .flex_1()
            .h(height)
            .rounded_full()
            .bg(track)
            .child(div().h_full().rounded_full().bg(color).w(relative(f.clamp(0., 1.))))
    }))
}

/// The one command that fits the current status: label, icon, and the status it leads to.
pub fn next_step(status: Status) -> (&'static str, IconName, Option<Status>) {
    match status {
        Status::Downloading | Status::Queued => ("Pause", IconName::Pause, Some(Status::Paused)),
        Status::Paused => ("Resume", IconName::Play, Some(Status::Downloading)),
        Status::Failed => ("Retry", IconName::RotateCw, Some(Status::Queued)),
        Status::Completed => ("Show in folder", IconName::FolderOpen, None),
        Status::Missing => ("Download again", IconName::RotateCw, Some(Status::Queued)),
    }
}

pub fn size_text(d: &Download) -> String {
    match d.total_bytes {
        Some(total) if matches!(d.status, Status::Completed | Status::Missing) => format_bytes(total),
        Some(total) => format!("{} of {}", format_bytes(d.received_bytes), format_bytes(total)),
        None if d.received_bytes > 0 => format_bytes(d.received_bytes),
        None => "—".into(),
    }
}

pub fn speed_text(d: &Download) -> String {
    if d.bytes_per_sec > 0 { format!("{}/s", format_bytes(d.bytes_per_sec)) } else { "—".into() }
}

pub fn eta_text(d: &Download) -> String {
    d.eta_secs().map(format_duration).unwrap_or_else(|| "—".into())
}

impl TableDelegate for DownloadsTable {
    fn columns_count(&self, _: &App) -> usize {
        ACTIONS + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        match col_ix {
            CHECK => Column::new("check", "").width(px(36.)).resizable(false).movable(false),
            // 712 = sum of the other columns; 12 leaves room for the vertical scrollbar.
            NAME => Column::new("name", "Name").width((self.width - px(712. + 12.)).max(px(180.))).min_width(px(180.)),
            PROGRESS => Column::new("progress", "Progress").width(px(170.)),
            SIZE => Column::new("size", "Size").width(px(150.)).text_right(),
            SPEED => Column::new("speed", "Speed").width(px(90.)).text_right(),
            ETA => Column::new("eta", "Time left").width(px(80.)).text_right(),
            STATUS => Column::new("status", "Status").width(px(110.)),
            ACTIONS => Column::new("actions", "").width(px(76.)).resizable(false),
            _ => unreachable!("columns_count is ACTIONS + 1"),
        }
    }

    fn render_tr(&mut self, row_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let id = self.rows[row_ix].id;
        div().id(("download", id)).when(self.checked.contains(&id), |this| this.bg(cx.theme().table_active))
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        if col_ix != CHECK {
            return div().size_full().child(self.column(col_ix, cx).name.clone()).into_any_element();
        }
        let all = !self.rows.is_empty() && self.rows.iter().all(|d| self.checked.contains(&d.id));
        let app = self.app.clone();
        Checkbox::new("check-all")
            .checked(all)
            .tooltip(if all { "Deselect all" } else { "Select all" })
            .on_change(move |value, _, cx| {
                cx.stop_propagation();
                app.update(cx, |this, cx| this.check_all(*value, cx)).ok();
            })
            .into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        let d = &self.rows[row_ix];
        Some(match col_ix {
            NAME => d.name.clone(),
            PROGRESS => format!("{:.0}%", d.progress()).into(),
            SIZE => size_text(d).into(),
            SPEED => speed_text(d).into(),
            ETA => eta_text(d).into(),
            STATUS => format!("{:?}", d.status).into(),
            _ => return None,
        })
    }

    fn render_empty(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Icon::new(IconName::Inbox).large().text_color(muted))
            .child(div().pt_1().font_weight(FontWeight::MEDIUM).child(self.empty.0))
            .child(div().text_xs().text_color(muted).child(self.empty.1))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let d = &self.rows[row_ix];
        let muted = cx.theme().muted_foreground;
        let id = d.id;
        match col_ix {
            CHECK => {
                let app = self.app.clone();
                Checkbox::new(("check", id))
                    .checked(self.checked.contains(&id))
                    .accessibility_label(format!("Select {}", d.name))
                    .on_change(move |_, _, cx| {
                        // Keep the row click from replacing the selection with this one row.
                        cx.stop_propagation();
                        app.update(cx, |this, cx| this.toggle_checked(id, cx)).ok();
                    })
                    .into_any_element()
            }
            NAME => h_flex()
                .gap_2()
                .min_w_0()
                .child(kind_icon(d.kind).small().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(d.name.clone()))
                .when(d.source == Source::YtDlp, |this| this.child(Tag::secondary().xsmall().child("yt-dlp")))
                .into_any_element(),
            PROGRESS => {
                let color = progress_color(d.status, cx);
                v_flex()
                    .w_full()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .w_full()
                            .child(
                                Progress::new(("progress", id))
                                    .value(d.progress())
                                    .color(color)
                                    .accessibility_label(format!("{} progress", d.name))
                                    .flex_1(),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .text_right()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{:.0}%", d.progress())),
                            ),
                    )
                    .when(!d.fragments.is_empty(), |this| this.child(fragment_strip(d, px(3.), cx).mr(px(40.))))
                    .into_any_element()
            }
            SIZE => div().text_color(muted).child(size_text(d)).into_any_element(),
            SPEED => div().text_color(muted).child(speed_text(d)).into_any_element(),
            ETA => div().text_color(muted).child(eta_text(d)).into_any_element(),
            STATUS => status_tag(d.status).xsmall().into_any_element(),
            _ => {
                let (label, icon, next) = next_step(d.status);
                let (app, app2) = (self.app.clone(), self.app.clone());
                let url = d.url.clone();
                h_flex()
                    .gap_0p5()
                    .child(Button::new(("step", id)).ghost().xsmall().icon(icon).tooltip(label).on_click(
                        move |_, window, cx| {
                            cx.stop_propagation();
                            app.update(cx, |this, cx| this.run_step(id, next, window, cx)).ok();
                        },
                    ))
                    .child(
                        Button::new(("more", id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Ellipsis)
                            .tooltip("More actions")
                            .dropdown_menu(move |menu, _, _| {
                                let (url, app) = (url.clone(), app2.clone());
                                menu.item(PopupMenuItem::new("Copy link").icon(IconName::Copy).on_click(
                                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(url.to_string())),
                                ))
                                .separator()
                                .item(
                                    PopupMenuItem::new("Remove from list").icon(IconName::Delete).on_click(
                                        move |_, _, cx| {
                                            app.update(cx, |this, cx| this.remove(id, cx)).ok();
                                        },
                                    ),
                                )
                            }),
                    )
                    .into_any_element()
            }
        }
    }
}
