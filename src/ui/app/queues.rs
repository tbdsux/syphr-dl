//! Queue commands, the scheduler tick, the finish countdown, and the Queues window that edits them.

use std::{cell::Cell, rc::Rc, time::Duration};

use chrono::{Local, NaiveDateTime, NaiveTime};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    h_flex,
    setting::{NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    switch::Switch,
    time_field::{TimeField, TimeFieldEvent, TimeFieldState},
    v_flex,
};
use gpui_kit::*;

use super::{Filter, SyphrApp};
use crate::download::Status;
use crate::queue::{EVERY_DAY, Finish, MAIN, Move, Queue};

/// How long a finished queue's action waits for a Cancel.
const COUNTDOWN_SECS: u32 = 30;
const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

impl SyphrApp {
    /// After every other download, so a new one starts last in its queue.
    pub(super) fn next_position(&self) -> i64 {
        self.downloads.iter().map(|d| d.position).max().unwrap_or(0) + 1
    }

    /// Starts what waits in the queue, paused downloads included.
    pub fn start_queue(&mut self, id: u64, cx: &mut Context<Self>) {
        set_status(&mut self.downloads, id, &[Status::Later, Status::Paused], Status::Queued);
        self.sync_table(cx);
    }

    /// Sends the queue's running and queued downloads back to waiting for it.
    pub fn stop_queue(&mut self, id: u64, cx: &mut Context<Self>) {
        set_status(&mut self.downloads, id, &[Status::Downloading, Status::Queued], Status::Later);
        self.sync_table(cx);
    }

    pub fn move_to_queue(&mut self, download: u64, queue: u64, cx: &mut Context<Self>) {
        let position = self.next_position();
        if let Some(d) = self.downloads.iter_mut().find(|d| d.id == download && d.queue != queue) {
            (d.queue, d.position) = (queue, position);
        }
        self.sync_table(cx);
    }

    pub fn reorder(&mut self, download: u64, to: Move, cx: &mut Context<Self>) {
        crate::queue::reorder(&mut self.downloads, download, to);
        self.sync_table(cx);
    }

    /// Checks each queue's schedule; a window opening starts the queue and one closing stops it.
    /// Returns whether any download changed.
    pub(super) fn run_schedules(&mut self, now: NaiveDateTime) -> bool {
        let mut changed = false;
        for q in &self.queues {
            let inside = q.inside(now);
            let was = self.in_window.insert(q.id, inside);
            // On the first tick too, so a window that opened while Syphr was closed still starts.
            if inside && was != Some(true) && q.start.is_some() {
                changed |= set_status(&mut self.downloads, q.id, &[Status::Later, Status::Paused], Status::Queued);
            } else if !inside && was == Some(true) && q.stop.is_some() {
                changed |= set_status(&mut self.downloads, q.id, &[Status::Downloading, Status::Queued], Status::Later);
            }
        }
        changed
    }

    fn save_queues(&mut self, cx: &mut Context<Self>) {
        self.store.save_queues(&self.queues);
        cx.notify();
    }

    /// Adds an empty queue and returns its id.
    pub fn add_queue(&mut self, cx: &mut Context<Self>) -> u64 {
        let id = self.queues.iter().map(|q| q.id).max().unwrap_or(MAIN) + 1;
        let n = (1..).find(|n| !self.queues.iter().any(|q| q.name == format!("Queue {n}"))).unwrap_or(1);
        self.queues.push(Queue::new(id, format!("Queue {n}")));
        self.save_queues(cx);
        id
    }

    pub fn edit_queue(&mut self, id: u64, edit: impl FnOnce(&mut Queue), cx: &mut Context<Self>) {
        let Some(q) = self.queues.iter_mut().find(|q| q.id == id) else { return };
        edit(q);
        // An edit only sets the next start or stop; it never fires one itself.
        self.in_window.insert(id, q.inside(Local::now().naive_local()));
        self.save_queues(cx);
    }

    /// Its downloads move to Main.
    pub fn delete_queue(&mut self, id: u64, cx: &mut Context<Self>) {
        if id == MAIN {
            return;
        }
        self.queues.retain(|q| q.id != id);
        for d in self.downloads.iter_mut().filter(|d| d.queue == id) {
            d.queue = MAIN;
        }
        if self.filter == Filter::Queue(id) {
            self.filter = Filter::All;
        }
        self.save_queues(cx);
        self.sync_table(cx);
    }

    /// Runs the queue's finish action after a countdown the user can cancel.
    pub(super) fn queue_finished(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(q) = self.queues.iter().find(|q| q.id == id) else { return };
        let (finish, name) = (q.on_finish, q.name.clone());
        if finish == Finish::Nothing || self.countdown.is_some() {
            return;
        }
        // The window may be hidden while downloads run; the countdown has to be seen to be canceled.
        cx.activate(true);
        window.activate_window();
        let left = Rc::new(Cell::new(COUNTDOWN_SECS));
        let (shown, view) = (left.clone(), cx.entity());
        let (verb, now) = match finish {
            Finish::Quit => ("Quitting Syphr", "Quit now"),
            Finish::Sleep => ("Sleeping", "Sleep now"),
            _ => ("Shutting down", "Shut down now"),
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (on_ok, on_cancel) = (view.clone(), view.clone());
            alert
                .title(format!("{verb} in {} s", shown.get()))
                .description(format!("The “{name}” queue finished."))
                .confirm()
                .ok_text(now)
                .ok_variant(if finish == Finish::Quit { ButtonVariant::Primary } else { ButtonVariant::Danger })
                .cancel_text("Cancel")
                .on_ok(move |_, window, cx| {
                    on_ok.update(cx, |this, cx| this.finish_now(finish, window, cx));
                    true
                })
                .on_cancel(move |_, _, cx| {
                    on_cancel.update(cx, |this, _| this.countdown = None);
                    true
                })
        });
        self.countdown = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let go = this.update_in(cx, |this, window, cx| {
                    // Dismissed some other way, e.g. Escape: treat it as Cancel.
                    if !window.has_active_dialog(cx) {
                        this.countdown = None;
                        return false;
                    }
                    left.set(left.get().saturating_sub(1));
                    window.refresh();
                    if left.get() > 0 {
                        return true;
                    }
                    window.close_dialog(cx);
                    this.finish_now(finish, window, cx);
                    false
                });
                if !go.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    fn finish_now(&mut self, finish: Finish, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the countdown task here is fine: it is the one calling us, or it's being canceled.
        self.countdown.take();
        if finish == Finish::Quit {
            return cx.quit();
        }
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            if let Err(e) = cx.background_spawn(async move { finish.run_system() }).await {
                let message = format!("Couldn't {}: {e}", finish.label().to_lowercase());
                cx.update_window(handle, |_, window, cx| window.push_notification(message, cx)).ok();
            }
        })
        .detach();
    }

    /// One window for all queues; `select` opens it on that queue.
    pub fn open_queues(&mut self, select: Option<u64>, cx: &mut Context<Self>) {
        let page = select.and_then(|id| self.queues.iter().position(|q| q.id == id));
        if let Some(handle) = self.queues_window
            && handle
                .update(cx, |view, window, cx| {
                    if let (Some(page), Ok(view)) = (page, view.downcast::<QueuesView>()) {
                        view.update(cx, |view, cx| view.show(page, cx));
                    }
                    window.activate_window();
                })
                .is_ok()
        {
            return;
        }
        let app = cx.entity();
        cx.defer(move |cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(760.), px(560.)), cx)),
                window_min_size: Some(size(px(720.), px(480.))),
                ..TitleBar::window_options()
            };
            let view = app.clone();
            if let Ok((handle, _)) =
                gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| QueuesView::new(view, page.unwrap_or(0), cx)))
            {
                app.update(cx, |this, _| this.queues_window = Some(handle));
            }
        });
    }
}

/// Moves the queue's downloads in `from` to `to`; returns whether any changed.
fn set_status(downloads: &mut [crate::download::Download], queue: u64, from: &[Status], to: Status) -> bool {
    let mut changed = false;
    for d in downloads.iter_mut().filter(|d| d.queue == queue && from.contains(&d.status)) {
        d.status = to;
        d.bytes_per_sec = 0;
        changed = true;
    }
    changed
}

/// "Starts Tue 09:00", or what else the schedule does.
pub fn schedule_summary(q: &Queue, now: NaiveDateTime) -> String {
    let stop = q.stop.map(|t| format!(", stops at {}", t.format("%H:%M"))).unwrap_or_default();
    match (q.next_start(now), q.start) {
        (Some(at), _) if at.date() == now.date() => format!("Starts today at {}{stop}", at.format("%H:%M")),
        (Some(at), _) => format!("Starts {}{stop}", at.format("%a %-d %b at %H:%M")),
        (None, Some(_)) => "Won't start again: no day is chosen, or its date has passed".into(),
        (None, None) if q.stop.is_some() => format!("Stops at {}", q.stop.unwrap_or_default().format("%H:%M")),
        (None, None) => "Starts only when you start it".into(),
    }
}

pub struct QueuesView {
    app: WeakEntity<SyphrApp>,
    page: usize,
    /// Part of the settings' element id: bumping it rebuilds the page list on `page`. Field state is keyed
    /// by page index, so it has to be rebuilt whenever a queue is added or deleted.
    generation: usize,
    /// How many queues the pages were built for.
    count: usize,
    _observe: Subscription,
}

impl QueuesView {
    fn new(app: Entity<SyphrApp>, page: usize, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&app, |_, _, cx| cx.notify());
        let count = app.read(cx).queues.len();
        Self { app: app.downgrade(), page, generation: 0, count, _observe: observe }
    }

    fn show(&mut self, page: usize, cx: &mut Context<Self>) {
        (self.page, self.generation) = (page, self.generation + 1);
        cx.notify();
    }
}

fn get<T>(app: &WeakEntity<SyphrApp>, id: u64, cx: &App, f: impl FnOnce(&Queue) -> T) -> Option<T> {
    app.upgrade().and_then(|app| app.read(cx).queues.iter().find(|q| q.id == id).map(f))
}

fn edit(app: &WeakEntity<SyphrApp>, id: u64, cx: &mut App, f: impl FnOnce(&mut Queue)) {
    app.update(cx, |this, cx| this.edit_queue(id, f, cx)).ok();
}

/// A switch that turns a start or stop time on, beside the time; editing the time turns it on too.
fn time_field(
    app: WeakEntity<SyphrApp>,
    id: u64,
    key: &'static str,
    slot: fn(&mut Queue) -> &mut Option<NaiveTime>,
    default: NaiveTime,
) -> SettingField<SharedString> {
    SettingField::render(move |_, window, cx| {
        let value = get(&app, id, cx, |q| *slot(&mut q.clone()));
        let state = window.use_keyed_state((key, id as usize), cx, |window, cx| {
            let app = app.clone();
            cx.subscribe_self(move |_: &mut TimeFieldState, event: &TimeFieldEvent, cx| {
                let TimeFieldEvent::Change(t) = *event;
                edit(&app, id, cx, |q| *slot(q) = Some(t));
            })
            .detach();
            TimeFieldState::new(window, cx)
        });
        let shown = value.flatten().unwrap_or(default);
        if state.read(cx).time() != shown {
            state.update(cx, |state, cx| state.set_time(shown, window, cx));
        }
        let on = value.flatten().is_some();
        let app = app.clone();
        h_flex().gap_3().child(TimeField::new(&state)).child(Switch::new((key, id as usize)).checked(on).on_click(
            move |on, _, cx| {
                let on = *on;
                edit(&app, id, cx, |q| *slot(q) = on.then_some(shown));
            },
        ))
    })
}

fn days_field(app: WeakEntity<SyphrApp>, id: u64) -> SettingField<SharedString> {
    SettingField::render(move |_, _, cx| {
        let days = get(&app, id, cx, |q| q.days).unwrap_or(EVERY_DAY);
        let app = app.clone();
        ButtonGroup::new(("days", id as usize))
            .outline()
            .small()
            .multiple(true)
            .children(
                DAYS.iter()
                    .enumerate()
                    .map(|(i, day)| Button::new(("day", i)).label(*day).selected(days & (1 << i) != 0)),
            )
            .on_click(move |picked: &Vec<usize>, _, cx| {
                let days = picked.iter().fold(0u8, |mask, i| mask | (1 << i));
                edit(&app, id, cx, |q| q.days = days);
            })
    })
}

fn date_field(app: WeakEntity<SyphrApp>, id: u64) -> SettingField<SharedString> {
    SettingField::render(move |_, window, cx| {
        let date = get(&app, id, cx, |q| q.date).flatten();
        let state = window.use_keyed_state(("date", id as usize), cx, |window, cx| {
            let app = app.clone();
            cx.subscribe_self(move |state: &mut DatePickerState, _: &DatePickerEvent, cx| {
                if let Some(date) = state.date().start() {
                    edit(&app, id, cx, |q| q.date = Some(date));
                }
            })
            .detach();
            DatePickerState::new(window, cx)
        });
        if let Some(date) = date
            && state.read(cx).date().start() != Some(date)
        {
            state.update(cx, |state, cx| state.set_date(date, window, cx));
        }
        DatePicker::new(&state).w(rems(12.))
    })
}

fn queue_page(app: &WeakEntity<SyphrApp>, q: &Queue, now: NaiveDateTime, global: f64) -> SettingPage {
    let id = q.id;
    let a = || app.clone();
    let mut schedule = vec![
        SettingItem::new("Start at", time_field(a(), id, "start", |q| &mut q.start, NaiveTime::MIN))
            .description("Its waiting and paused downloads start."),
        SettingItem::new(
            "Stop at",
            time_field(a(), id, "stop", |q| &mut q.stop, NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default()),
        )
        .description("Its downloads stop and wait for the next start. Earlier than the start means the next day."),
        SettingItem::new(
            "Repeat",
            SettingField::dropdown(
                vec![("days".into(), "On chosen days".into()), ("once".into(), "Once, on a date".into())],
                {
                    let app = a();
                    move |cx: &App| {
                        let once = get(&app, id, cx, |q| q.date.is_some()).unwrap_or(false);
                        SharedString::from(if once { "once" } else { "days" })
                    }
                },
                {
                    let app = a();
                    move |value: SharedString, cx: &mut App| {
                        let today = Local::now().date_naive();
                        edit(&app, id, cx, |q| q.date = (value == "once").then_some(q.date.unwrap_or(today)));
                    }
                },
            ),
        ),
    ];
    schedule.push(match q.date {
        Some(_) => SettingItem::new("Date", date_field(a(), id)),
        None => SettingItem::new("Days", days_field(a(), id)).layout(Axis::Vertical),
    });
    let mut page = SettingPage::new(q.name.clone())
        .icon(Icon::new(if q.scheduled() { Lucide::CalendarClock } else { Lucide::ListOrdered }))
        .description(schedule_summary(q, now))
        .group(SettingGroup::new().item(SettingItem::new(
            "Name",
            SettingField::input(
                {
                    let app = a();
                    move |cx: &App| get(&app, id, cx, |q| SharedString::from(q.name.clone())).unwrap_or_default()
                },
                {
                    let app = a();
                    move |value: SharedString, cx: &mut App| edit(&app, id, cx, |q| q.name = value.trim().to_string())
                },
            ),
        )))
        .group(SettingGroup::new().title("Schedule").items(schedule))
        .group(
            SettingGroup::new().title("Downloads").items([
                SettingItem::new(
                    "At once",
                    SettingField::number_input(
                        NumberFieldOptions { min: 0., max: 10., step: 1. },
                        {
                            let app = a();
                            move |cx: &App| get(&app, id, cx, |q| q.max_concurrent as f64).unwrap_or(0.)
                        },
                        {
                            let app = a();
                            move |value: f64, cx: &mut App| {
                                edit(&app, id, cx, |q| q.max_concurrent = value.clamp(0., 10.) as u32)
                            }
                        },
                    ),
                )
                .description(format!(
                    "0 follows Settings → Simultaneous downloads ({global}), which also caps every queue together."
                )),
                SettingItem::new(
                    "When it finishes",
                    SettingField::dropdown(
                        Finish::ALL.iter().map(|f| (f.key().into(), f.label().into())).collect(),
                        {
                            let app = a();
                            move |cx: &App| {
                                get(&app, id, cx, |q| SharedString::from(q.on_finish.key())).unwrap_or_default()
                            }
                        },
                        {
                            let app = a();
                            move |value: SharedString, cx: &mut App| {
                                edit(&app, id, cx, |q| q.on_finish = Finish::from_key(&value))
                            }
                        },
                    ),
                )
                .description(format!(
                    "Runs once nothing in the queue is left to download. You get {COUNTDOWN_SECS} seconds to cancel."
                )),
            ]),
        );
    if id != MAIN {
        let (app, name) = (a(), q.name.clone());
        page = page.title_suffix(move |_, _| {
            let (app, name) = (app.clone(), name.clone());
            Button::new(("delete-queue", id as usize)).small().ghost().icon(IconName::Delete).label("Delete…").on_click(
                move |_, window, cx| {
                    let (app, name) = (app.clone(), name.clone());
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let app = app.clone();
                        alert
                            .title(format!("Delete “{name}”?"))
                            .description("Its downloads move to Main.")
                            .confirm()
                            .ok_text("Delete")
                            .ok_variant(ButtonVariant::Danger)
                            .on_ok(move |_, _, cx| {
                                app.update(cx, |this, cx| this.delete_queue(id, cx)).ok();
                                true
                            })
                    });
                },
            )
        });
    }
    page
}

impl Render for QueuesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = cx.theme().sidebar;
        let queues = self.app.upgrade().map(|app| app.read(cx).queues.clone()).unwrap_or_default();
        // A deleted queue's page is gone: rebuild so field state stays with the right queue.
        if queues.len() != self.count {
            self.count = queues.len();
            self.generation += 1;
            self.page = self.page.min(queues.len().saturating_sub(1));
        }
        let now = Local::now().naive_local();
        let global = crate::ui::settings::AppSettings::get(cx).max_concurrent;
        let pages: Vec<SettingPage> = queues.iter().map(|q| queue_page(&self.app, q, now, global)).collect();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().bg(chrome).border_color(chrome).child(
                h_flex().w_full().pr_2().justify_between().child("Queues").child(
                    div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                        Button::new("new-queue").small().ghost().icon(IconName::Plus).label("New queue").on_click(
                            cx.listener(|this, _, _, cx| {
                                let Some(app) = this.app.upgrade() else { return };
                                let id = app.update(cx, |app, cx| app.add_queue(cx));
                                let page = app.read(cx).queues.iter().position(|q| q.id == id).unwrap_or(0);
                                this.show(page, cx);
                            }),
                        ),
                    ),
                ),
            ))
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new(("queues", self.generation))
                        .sidebar_width(cx.theme().font_size * 15.)
                        .default_selected_index(SelectIndex { page_ix: self.page, group_ix: None })
                        .pages(pages),
                ),
            )
    }
}
