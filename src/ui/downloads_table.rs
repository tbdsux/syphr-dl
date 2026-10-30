use gpui_kit::assets::IconName as Lucide;
use std::{cmp::Ordering, collections::BTreeSet};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
    table::{Column, ColumnSort, TableDelegate, TableState},
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
const IDS: [&str; 8] = ["check", "name", "progress", "size", "speed", "eta", "status", "actions"];
const NAMES: [&str; 8] = ["", "Name", "Progress", "Size", "Speed", "Time left", "Status", ""];
/// Default widths in rems, so columns grow with the text size setting. See `layout` for Name and Actions.
const WIDTHS: [f32; 8] = [2.75, 14., 13., 11.5, 7., 6., 8.5, 5.75];
const NAME_MIN: f32 = 14.;
const COL_MIN: f32 = 4.;
/// Columns the user can hide in Settings, by id and label.
pub const OPTIONAL_COLUMNS: [(&str, &str); 5] =
    [("progress", "Progress"), ("size", "Size"), ("speed", "Speed"), ("eta", "Time left"), ("status", "Status")];

/// Rows are a filtered snapshot that `SyphrApp` refreshes whenever its list changes.
pub struct DownloadsTable {
    pub rows: Vec<Download>,
    pub checked: BTreeSet<u64>,
    /// Title, hint, and whether to offer New download.
    pub empty: (&'static str, &'static str, bool),
    /// Measured table width; see `layout`.
    pub width: Pixels,
    /// The text size the columns were laid out for; they're sized in rems.
    pub rem: Pixels,
    /// Sorted column and direction; None keeps the newest-added first.
    pub sort: Option<(usize, ColumnSort)>,
    /// Every column in the user's order; `cols` is the visible part of it, indexed by table column.
    order: Vec<usize>,
    cols: Vec<usize>,
    /// Each column's width in rems, as the user resized it.
    widths: [f32; 8],
    /// Whether the user resized a column yet; until then Name fills the table.
    resized: bool,
    /// Columns changed and the table must rebuild them. Only then: a rebuild mid-drag resets the
    /// kit's column bounds, and its resize math then jumps the dragged column.
    pub stale: bool,
    /// The table column `fit_col` is fitting for `follow_drag`, and the width to give it.
    fit: Option<(usize, Pixels)>,
    /// `ColumnWidthsChanged` events caused by `follow_drag` itself, which must not rebuild the columns.
    pub own_fits: usize,
    app: WeakEntity<SyphrApp>,
}

impl DownloadsTable {
    pub fn new(app: WeakEntity<SyphrApp>) -> Self {
        Self {
            rows: Vec::new(),
            checked: BTreeSet::new(),
            empty: ("", "", false),
            width: px(0.),
            rem: px(0.),
            sort: None,
            order: (CHECK..=ACTIONS).collect(),
            cols: (CHECK..=ACTIONS).collect(),
            widths: WIDTHS,
            resized: false,
            stale: true,
            fit: None,
            own_fits: 0,
            app,
        }
    }

    pub fn set_hidden(&mut self, hidden: &BTreeSet<String>) {
        let cols: Vec<usize> = self.order.iter().copied().filter(|&c| !hidden.contains(IDS[c])).collect();
        if cols != self.cols {
            self.cols = cols;
            self.stale = true;
        }
    }

    /// Keeps the widths the user dragged to; `px` is indexed by table column.
    pub fn set_widths(&mut self, px: &[Pixels], rem: Pixels) {
        let laid_out = self.layout(rem);
        for ((&c, &w), &was) in self.cols.iter().zip(px).zip(&laid_out) {
            // Tolerance: rems → px → rems doesn't round-trip exactly. Actions only takes the slack.
            if c != ACTIONS && (was - w / rem).abs() > 0.01 {
                self.resized = true;
                self.stale = true;
            }
        }
        if self.stale {
            // Name's stored width was only its floor while it filled, so take what every column is drawn at.
            for (&c, &w) in self.cols.iter().zip(px).filter(|(c, _)| **c != ACTIONS) {
                self.widths[c] = w / rem;
            }
        }
    }

    /// Each visible column's width in rems. Until the user resizes a column Name fills the table; after,
    /// Actions takes the slack, so an edge drag only moves the columns to its right. Name gives width
    /// back when the window gets too narrow for the rest.
    fn layout(&self, rem: Pixels) -> Vec<f32> {
        // 12px leaves room for the vertical scrollbar.
        let room = (self.width - px(12.)) / rem;
        let mut w: Vec<f32> = self.cols.iter().map(|&c| self.widths[c]).collect();
        let mut slack = room - w.iter().sum::<f32>();
        let name = self.cols.iter().position(|&c| c == NAME).expect("Name can't be hidden");
        let give = if self.resized { slack.min(0.) } else { slack };
        let give = give.max(NAME_MIN - w[name]);
        w[name] += give;
        slack -= give;
        *w.last_mut().expect("Actions is always last") += slack.max(0.);
        w
    }

    /// Mid-drag, gives the dragged column's change to Actions so the table never runs past its edge.
    /// It resizes through `fit_col`: `refresh` resets the kit's column bounds and breaks the drag.
    pub fn follow_drag(table: &mut TableState<Self>, window: &mut Window, cx: &mut Context<TableState<Self>>) {
        let rem = cx.theme().font_size;
        let want: Vec<Pixels> = table.delegate().layout(rem).into_iter().map(|w| rem * w).collect();
        let drawn: Vec<Pixels> = (0..want.len()).filter_map(|ix| table.col_width(ix)).collect();
        let last = want.len() - 1;
        if drawn.len() != want.len() {
            return;
        }
        let target = want.iter().sum::<Pixels>() - drawn[..last].iter().copied().sum::<Pixels>();
        if (drawn[last] - target).abs() > px(0.5) {
            table.delegate_mut().fit = Some((last, target));
            table.fit_col(last, window, cx);
            table.delegate_mut().fit = None;
            if table.col_width(last) != Some(drawn[last]) {
                table.delegate_mut().own_fits += 1;
            }
        }
    }

    pub fn sort_rows(&mut self) {
        let Some((col, dir)) = self.sort else { return self.rows.sort_by_key(|d| std::cmp::Reverse(d.id)) };
        let size = |d: &Download| d.total_bytes.unwrap_or(d.received_bytes);
        self.rows.sort_by(|a, b| {
            let ord = match col {
                NAME => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                PROGRESS => a.progress().total_cmp(&b.progress()),
                SIZE => size(a).cmp(&size(b)),
                SPEED => a.bytes_per_sec.cmp(&b.bytes_per_sec),
                ETA => a.eta_secs().cmp(&b.eta_secs()),
                STATUS => a.status.cmp(&b.status),
                _ => Ordering::Equal,
            };
            // Ties keep the newest-added first, in both directions.
            (if dir == ColumnSort::Descending { ord.reverse() } else { ord }).then(b.id.cmp(&a.id))
        });
    }

    fn move_col(&mut self, col_ix: usize, to_ix: usize) {
        // Checkboxes stay first and actions last, so a column can't be dropped past either edge.
        let to_ix = to_ix.clamp(1, self.cols.len() - 2);
        if to_ix == col_ix {
            return;
        }
        // Mirror the table's remove-then-insert on the full order, so hidden columns keep their place.
        let (from, to) = (self.cols[col_ix], self.cols[to_ix]);
        let at = self.order.iter().position(|&c| c == to).expect("visible columns are in order");
        self.order.retain(|&c| c != from);
        self.order.insert(at, from);
        let c = self.cols.remove(col_ix);
        self.cols.insert(to_ix, c);
    }

    fn set_sort(&mut self, col: usize, sort: ColumnSort, cx: &mut Context<TableState<Self>>) {
        self.sort = (sort != ColumnSort::Default).then_some((col, sort));
        self.stale = true;
        self.resync(cx);
    }

    /// The table is mid-update when it calls the delegate; resync after so rows and columns match its state.
    fn resync(&self, cx: &mut Context<TableState<Self>>) {
        let app = self.app.clone();
        cx.defer(move |cx| {
            app.update(cx, |this, cx| this.sync_table(cx)).ok();
        });
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
pub fn fragment_strip(d: &Download, height: Rems, cx: &App) -> Div {
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
        self.cols.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, cx: &App) -> Column {
        let rem = cx.theme().font_size;
        let c = self.cols[col_ix];
        let w = self.layout(rem);
        // Grow only into the slack Actions holds, so no column gets pushed off the right edge.
        let spare = w[w.len() - 1] - WIDTHS[ACTIONS];
        let min = match c {
            NAME => NAME_MIN,
            CHECK | ACTIONS => WIDTHS[c],
            _ => COL_MIN,
        };
        let column = Column::new(IDS[c], NAMES[c])
            .width(rem * w[col_ix])
            .min_width(rem * min)
            .max_width(rem * (w[col_ix] + spare));
        let sort = match self.sort {
            Some((s, dir)) if s == c => dir,
            _ => ColumnSort::Default,
        };
        match c {
            CHECK => column.resizable(false).movable(false),
            // Resizable only so `follow_drag` can use `fit_col`; its own edge, the table's, snaps back.
            ACTIONS => column.min_width(rem * WIDTHS[ACTIONS]).max_width(px(f32::MAX)).movable(false),
            SIZE | SPEED | ETA => column.text_right().sort(sort),
            _ => column.sort(sort),
        }
    }

    fn fit_column(&mut self, col_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> Option<Pixels> {
        // `fit_col` adds the small table's cell padding (6px each side) back on. Double-clicks get None.
        self.fit.filter(|&(ix, _)| ix == col_ix).map(|(_, w)| w - px(12.))
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.set_sort(self.cols[col_ix], sort, cx);
    }

    fn move_column(&mut self, col_ix: usize, to_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.move_col(col_ix, to_ix);
        // The table already moved its headers to the unclamped spot; rebuild them from `cols`.
        self.stale = true;
        self.resync(cx);
    }

    fn render_tr(&mut self, row_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let id = self.rows[row_ix].id;
        div().id(("download", id)).when(self.checked.contains(&id), |this| this.bg(cx.theme().table_active))
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let c = self.cols[col_ix];
        if c == ACTIONS {
            return div().into_any_element();
        }
        if c != CHECK {
            // The whole header sorts, not just the kit's small sort icon.
            return div()
                .id(("th", c))
                .size_full()
                .child(NAMES[c])
                .on_click(cx.listener(move |table, _, _, cx| {
                    cx.stop_propagation();
                    let d = table.delegate_mut();
                    let next = match d.sort {
                        Some((s, ColumnSort::Descending)) if s == c => ColumnSort::Ascending,
                        Some((s, ColumnSort::Ascending)) if s == c => ColumnSort::Default,
                        _ => ColumnSort::Descending,
                    };
                    d.set_sort(c, next, cx);
                }))
                .into_any_element();
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
        Some(match self.cols[col_ix] {
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
        let theme = cx.theme();
        let (title, hint, offer_new) = self.empty;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_12()
                    .mb_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.secondary)
                    .child(Icon::new(IconName::Inbox).large().text_color(theme.muted_foreground)),
            )
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(theme.muted_foreground).child(hint))
            .when(offer_new, |this| {
                this.child(
                    Button::new("empty-new")
                        .mt_3()
                        .outline()
                        .icon(IconName::Plus)
                        .label("New download")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::NewDownload), cx)),
                )
            })
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
        match self.cols[col_ix] {
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
                                    .w(rems(2.5))
                                    .text_right()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{:.0}%", d.progress())),
                            ),
                    )
                    .when(!d.fragments.is_empty(), |this| this.child(fragment_strip(d, rems(0.25), cx).mr(rems(3.))))
                    .into_any_element()
            }
            SIZE => div().text_color(muted).child(size_text(d)).into_any_element(),
            SPEED => div().text_color(muted).child(speed_text(d)).into_any_element(),
            ETA => div().text_color(muted).child(eta_text(d)).into_any_element(),
            STATUS => status_tag(d.status).xsmall().into_any_element(),
            _ => {
                let (label, icon, next) = next_step(d.status);
                let (app, app2) = (self.app.clone(), self.app.clone());
                let (url, saved) = (d.url.clone(), d.path.is_some());
                // Right-aligned: this column takes the table's slack once a column is resized.
                h_flex()
                    .w_full()
                    .justify_end()
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
                                .item(PopupMenuItem::new("Remove from list").icon(IconName::Delete).on_click({
                                    let app = app.clone();
                                    move |_, _, cx| {
                                        app.update(cx, |this, cx| this.remove(id, cx)).ok();
                                    }
                                }))
                                .when(saved, |menu| {
                                    menu.item(
                                        PopupMenuItem::new("Remove and delete file…").icon(IconName::Delete).on_click(
                                            move |_, window, cx| {
                                                app.update(cx, |this, cx| this.confirm_delete(id, window, cx)).ok();
                                            },
                                        ),
                                    )
                                })
                            }),
                    )
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ACTIONS, CHECK, ColumnSort, DownloadsTable, ETA, NAME, PROGRESS, SIZE, SPEED, STATUS};
    use crate::download::{Download, Kind, Source, Status};
    use gpui_kit::WeakEntity;
    use std::collections::BTreeSet;

    fn row(id: u64, name: &str, status: Status) -> Download {
        Download {
            id,
            name: name.to_string().into(),
            url: "".into(),
            kind: Kind::Archive,
            source: Source::Direct,
            status,
            total_bytes: None,
            received_bytes: 0,
            bytes_per_sec: 0,
            folder: "".into(),
            format: None,
            selector: None,
            file_name: None,
            path: None,
            fragments: Vec::new(),
        }
    }

    #[test]
    fn sorts_and_hides_columns() {
        let mut t = DownloadsTable::new(WeakEntity::new_invalid());
        t.rows = vec![row(1, "b", Status::Failed), row(3, "a", Status::Completed), row(2, "C", Status::Completed)];
        let ids = |t: &DownloadsTable| t.rows.iter().map(|d| d.id).collect::<Vec<_>>();
        t.sort_rows();
        assert_eq!(ids(&t), [3, 2, 1]);
        t.sort = Some((NAME, ColumnSort::Ascending));
        t.sort_rows();
        assert_eq!(ids(&t), [3, 1, 2]);
        t.sort = Some((STATUS, ColumnSort::Descending));
        t.sort_rows();
        assert_eq!(ids(&t), [1, 3, 2]);

        t.set_hidden(&BTreeSet::from(["size".to_string()]));
        assert_eq!(t.cols, [CHECK, NAME, PROGRESS, SPEED, ETA, STATUS, ACTIONS]);
        // Drag Status before Progress, then show Size again: it stays after Progress.
        t.move_col(5, 2);
        assert_eq!(t.cols, [CHECK, NAME, STATUS, PROGRESS, SPEED, ETA, ACTIONS]);
        // Past either edge lands just inside the checkbox and actions columns.
        t.move_col(1, 6);
        assert_eq!(t.cols, [CHECK, STATUS, PROGRESS, SPEED, ETA, NAME, ACTIONS]);
        t.move_col(5, 0);
        assert_eq!(t.cols, [CHECK, NAME, STATUS, PROGRESS, SPEED, ETA, ACTIONS]);
        t.set_hidden(&BTreeSet::new());
        assert_eq!(t.cols, [CHECK, NAME, STATUS, PROGRESS, SIZE, SPEED, ETA, ACTIONS]);
    }
}
