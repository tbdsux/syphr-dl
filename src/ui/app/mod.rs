use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme, WindowExt,
    button::ButtonVariant,
    input::{InputEvent, InputState},
    menu::AppMenuBar,
    table::{TableEvent, TableState},
};
use gpui_kit::*;

use crate::download::{Download, Kind, Source, Status};
use crate::engine::{Job, Request};
use crate::queue::{MAIN, Queue};
use crate::store::Store;
use crate::ui::add_download::{AddDownload, AddEvent, kind_for};
use crate::ui::downloads_table::DownloadsTable;
use crate::ui::settings::AppSettings;

mod details;
mod queues;
mod selection;
mod views;

use selection::next_selection;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Completed,
    Failed,
    Kind(Kind),
    Queue(u64),
}

impl Filter {
    fn title(self) -> &'static str {
        match self {
            Filter::All => "All downloads",
            Filter::Active => "Active",
            Filter::Completed => "Completed",
            Filter::Failed => "Failed",
            Filter::Kind(Kind::Video) => "Video",
            Filter::Kind(Kind::Music) => "Music",
            Filter::Kind(Kind::Document) => "Documents",
            Filter::Kind(Kind::Program) => "Programs",
            Filter::Kind(Kind::Archive) => "Archives",
            Filter::Queue(_) => "Queue",
        }
    }

    fn matches(self, d: &Download) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => {
                matches!(d.status, Status::Downloading | Status::Paused | Status::Queued | Status::Later)
            }
            Filter::Completed => matches!(d.status, Status::Completed | Status::Missing),
            Filter::Failed => d.status == Status::Failed,
            Filter::Kind(kind) => d.kind == kind,
            Filter::Queue(id) => d.queue == id,
        }
    }
}

pub struct SyphrApp {
    downloads: Vec<Download>,
    filter: Filter,
    /// Selected downloads by id.
    checked: BTreeSet<u64>,
    /// Where a Shift-click range starts.
    anchor: Option<u64>,
    /// `SelectRow` events caused by `sync_table` itself, which must not change the selection.
    own_row_events: usize,
    search: Entity<InputState>,
    add: Entity<AddDownload>,
    table: Entity<TableState<DownloadsTable>>,
    /// The in-window menus on Windows and Linux.
    menu_bar: Option<Entity<AppMenuBar>>,
    focus: FocusHandle,
    next_id: u64,
    /// Engine state per download id; kept while paused so a resume continues.
    jobs: HashMap<u64, Job>,
    store: Store,
    /// Open details windows by download id.
    details: HashMap<u64, AnyWindowHandle>,
    queues: Vec<Queue>,
    /// Whether each queue was inside its schedule window on the last tick; a change starts or stops it.
    in_window: HashMap<u64, bool>,
    queues_window: Option<AnyWindowHandle>,
    /// The countdown before a finished queue's action; dropping it cancels the action.
    countdown: Option<Task<()>>,
    _pump: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl SyphrApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let add = cx.new(|cx| AddDownload::new(window, cx));
        let weak = cx.entity().downgrade();
        let table = cx.new(|cx| TableState::new(DownloadsTable::new(weak), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&add, window, |this, _, event, window, cx| match event {
                AddEvent::Submit => this.submit_add(false, window, cx),
                AddEvent::Later => this.submit_add(true, window, cx),
            }),
            cx.observe(&add, |_, _, cx| cx.notify()),
            cx.observe_global::<AppSettings>(|this, cx| this.sync_table(cx)),
            cx.subscribe(&search, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_table(cx);
                }
            }),
            cx.subscribe_in(&table, window, |this, table, event, window, cx| {
                match event {
                    TableEvent::SelectRow(ix) => {
                        if this.own_row_events > 0 {
                            this.own_row_events -= 1;
                            return;
                        }
                        let ids: Vec<u64> = table.read(cx).delegate().rows.iter().map(|d| d.id).collect();
                        let modifiers = window.modifiers();
                        this.select_row(&ids, *ix, modifiers.shift, modifiers.secondary(cx), cx);
                    }
                    TableEvent::DoubleClickedRow(ix) => {
                        if let Some(id) = table.read(cx).delegate().rows.get(*ix).map(|d| d.id) {
                            this.open_details(id, cx);
                        }
                    }
                    // Sent once the drag ends; Name then gives up or takes back the difference.
                    TableEvent::ColumnWidthsChanged(widths) => {
                        let rem = cx.theme().font_size;
                        let own = table.update(cx, |table, _| {
                            let d = table.delegate_mut();
                            if d.own_fits > 0 {
                                d.own_fits -= 1;
                                return true;
                            }
                            d.set_widths(widths, rem);
                            false
                        });
                        if !own {
                            this.sync_table(cx);
                        }
                    }
                    _ => {}
                }
            }),
        ];
        let pump = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if this.update_in(cx, |this, window, cx| this.pump(window, cx)).is_err() {
                    break;
                }
            }
        });
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let active = this.read_with(cx, |this, _| this.has_active()).unwrap_or(false);
            close_main_window(active, window, cx);
            false
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut store = Store::open_default();
        let downloads = store.load().unwrap_or_else(|err| {
            eprintln!("Couldn't read the download list: {err}");
            Vec::new()
        });
        let queues = store.load_queues().unwrap_or_else(|err| {
            eprintln!("Couldn't read the queues: {err}");
            vec![Queue::new(MAIN, "Main")]
        });
        let mut this = Self {
            next_id: store.next_id(),
            downloads,
            store,
            details: HashMap::new(),
            queues,
            in_window: HashMap::new(),
            queues_window: None,
            countdown: None,
            jobs: HashMap::new(),
            _pump: pump,
            filter: Filter::All,
            checked: BTreeSet::new(),
            anchor: None,
            own_row_events: 0,
            search,
            add,
            table,
            menu_bar: (!cfg!(target_os = "macos")).then(|| AppMenuBar::new(cx)),
            focus,
            _subscriptions: subscriptions,
        };
        this.sync_table(cx);
        this
    }

    pub fn downloads(&self) -> &[Download] {
        &self.downloads
    }

    /// Running or queued work, or work waiting for a schedule, which needs the app running to start.
    fn has_active(&self) -> bool {
        self.downloads.iter().any(|d| match d.status {
            Status::Downloading | Status::Queued => true,
            Status::Later => self.queues.iter().any(|q| q.id == d.queue && q.scheduled()),
            _ => false,
        })
    }

    /// Close Window and the Linux close button, which bypass the platform's should-close check.
    fn close(&mut self, window: &mut Window, cx: &mut App) {
        close_main_window(self.has_active(), window, cx);
    }

    /// Pushes the filtered list into the table and keeps the selection on the same download.
    pub fn sync_table(&mut self, cx: &mut Context<Self>) {
        self.store.sync(&self.downloads);
        let query = self.search.read(cx).value().to_lowercase();
        let rows: Vec<Download> = self
            .downloads
            .iter()
            .filter(|d| self.filter.matches(d) && d.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let empty = if self.downloads.is_empty() {
            ("No downloads yet", "Paste a file link or a video page URL to start.", true)
        } else {
            ("Nothing here", "Try another category or clear the search.", false)
        };
        // Selection only covers what is on screen, so hidden rows are never acted on by accident.
        self.checked.retain(|id| rows.iter().any(|d| d.id == *id));
        let anchor = self.anchor.filter(|id| self.checked.contains(id));
        let checked = self.checked.clone();
        let hidden = AppSettings::get(cx).hidden_columns.clone();
        let queue_order = matches!(self.filter, Filter::Queue(_));
        let queues: Vec<(u64, SharedString)> = self.queues.iter().map(|q| (q.id, q.name.clone().into())).collect();
        let mut own_events = 0;
        self.table.update(cx, |table, cx| {
            let d = table.delegate_mut();
            d.rows = rows;
            d.queue_order = queue_order;
            d.queues = queues;
            d.sort_rows();
            d.set_hidden(&hidden);
            d.checked = checked;
            d.empty = empty;
            let anchor_ix = anchor.and_then(|id| d.rows.iter().position(|d| d.id == id));
            // Leave the keyboard cursor alone while it sits on a selected row; otherwise move it to the anchor.
            let rows = &table.delegate().rows;
            let cursor_ok = table
                .selected_row()
                .and_then(|r| rows.get(r))
                .is_some_and(|d| table.delegate().checked.contains(&d.id));
            match anchor_ix {
                _ if cursor_ok => {}
                Some(ix) => {
                    own_events += 1;
                    table.set_selected_row(ix, cx);
                }
                None => table.clear_selection(cx),
            }
            if std::mem::take(&mut table.delegate_mut().stale) {
                table.refresh(cx);
            }
            cx.notify();
        });
        self.own_row_events += own_events;
        cx.notify();
    }

    /// Finder-style: click selects one, ⌘-click toggles, Shift-click (or Shift+arrow) extends from the anchor.
    fn select_row(&mut self, ids: &[u64], ix: usize, shift: bool, toggle: bool, cx: &mut Context<Self>) {
        if let Some((checked, anchor)) = next_selection(&self.checked, self.anchor, ids, ix, shift, toggle) {
            (self.checked, self.anchor) = (checked, anchor);
            self.sync_table(cx);
        }
    }

    pub fn toggle_checked(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.checked.remove(&id) {
            self.checked.insert(id);
        }
        self.anchor = Some(id);
        self.sync_table(cx);
    }

    pub fn check_all(&mut self, all: bool, cx: &mut Context<Self>) {
        self.checked =
            if all { self.table.read(cx).delegate().rows.iter().map(|d| d.id).collect() } else { BTreeSet::new() };
        self.sync_table(cx);
    }

    fn clear_checked(&mut self, cx: &mut Context<Self>) {
        self.checked.clear();
        self.anchor = None;
        self.sync_table(cx);
    }

    fn checked_with(&self, statuses: &[Status]) -> usize {
        self.downloads.iter().filter(|d| self.checked.contains(&d.id) && statuses.contains(&d.status)).count()
    }

    fn set_checked_status(&mut self, from: &[Status], to: Status, cx: &mut Context<Self>) {
        for d in self.downloads.iter_mut().filter(|d| self.checked.contains(&d.id) && from.contains(&d.status)) {
            d.status = to;
            d.bytes_per_sec = 0;
        }
        self.sync_table(cx);
    }

    fn copy_checked_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let links: Vec<&str> =
            self.downloads.iter().filter(|d| self.checked.contains(&d.id)).map(|d| d.url.as_ref()).collect();
        cx.write_to_clipboard(ClipboardItem::new_string(links.join("\n")));
        window.push_notification(format!("Copied {} links", links.len()), cx);
    }

    /// Space: pauses what is running in the selection, or resumes it when nothing is.
    fn toggle_pause_checked(&mut self, cx: &mut Context<Self>) {
        if self.checked_with(&[Status::Downloading, Status::Queued]) > 0 {
            self.set_checked_status(&[Status::Downloading, Status::Queued], Status::Paused, cx);
        } else {
            self.set_checked_status(&[Status::Paused, Status::Later], Status::Downloading, cx);
        }
    }

    fn confirm_remove_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.checked.len();
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title(format!("Remove {count} downloads from the list?"))
                .description("Files already saved to disk aren't deleted.")
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        let checked = std::mem::take(&mut this.checked);
                        this.remove_rows(&checked, cx);
                    });
                    true
                })
        });
    }

    pub fn confirm_delete(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.downloads.iter().find(|d| d.id == id) else { return };
        let (name, path) = (d.name.clone(), d.path.clone());
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, path) = (view.clone(), path.clone());
            alert
                .title(format!("Delete “{name}”?"))
                .description("Removes it from the list and deletes the file from disk.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    if let Some(path) = &path
                        && let Err(e) = std::fs::remove_file(path)
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        window.push_notification(format!("Couldn't delete the file: {e}"), cx);
                        return true;
                    }
                    view.update(cx, |this, cx| this.remove(id, cx));
                    true
                })
        });
    }

    pub fn run_step(&mut self, id: u64, next: Option<Status>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = next else {
            let path = self.downloads.iter().find(|d| d.id == id).and_then(|d| d.path.clone());
            match path.filter(|p| p.exists()) {
                Some(path) => {
                    if let Err(e) = reveal(&path) {
                        window.push_notification(format!("Couldn't show the file: {e}"), cx);
                    }
                }
                None => window.push_notification("The file is no longer where it was saved.", cx),
            }
            return;
        };
        if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
            d.status = next;
            if next != Status::Downloading {
                d.bytes_per_sec = 0;
            }
        }
        self.sync_table(cx);
    }

    /// Starts queued work up to the concurrency limit, stops work the user paused, and copies progress into the rows.
    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::get(cx);
        let max = settings.max_concurrent.max(1.) as usize;
        let limit_bps = (settings.speed_limit_kbps.max(0.) * 1024.) as u64;
        let fragments = settings.fragments.max(1.) as u64;
        let (bin, ffmpeg, notify) = (
            crate::tools::resolve(&settings.ytdlp_path),
            crate::tools::resolve(&settings.ffmpeg_path),
            settings.notify_on_complete,
        );
        let mut active = self.jobs.values().filter(|j| j.is_running()).count();
        let mut changed = self.run_schedules(chrono::Local::now().naive_local());
        let mut in_queue: HashMap<u64, usize> = HashMap::new();
        for d in self.downloads.iter().filter(|d| self.jobs.get(&d.id).is_some_and(Job::is_running)) {
            *in_queue.entry(d.queue).or_default() += 1;
        }
        // Queues that had a download end this tick; any left with nothing to run have finished.
        let mut ended = BTreeSet::new();
        // In queue order, so each queue runs its downloads in the order the user arranged.
        for i in crate::queue::start_order(&self.downloads) {
            let d = &mut self.downloads[i];
            let job = self.jobs.entry(d.id).or_default();
            for line in job.take_log() {
                self.store.log(d.id, &line);
            }
            if let Some(result) = job.take_result() {
                changed = true;
                ended.insert(d.queue);
                d.bytes_per_sec = 0;
                match result {
                    Ok(path) => {
                        d.status = Status::Completed;
                        d.received_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(d.received_bytes);
                        d.total_bytes = Some(d.received_bytes);
                        d.fragments.clear();
                        d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into();
                        self.store.log(d.id, &format!("Saved to {}", path.display()));
                        d.path = Some(path);
                        if notify {
                            announce(d.id, "Download complete", d.name.clone(), window, cx);
                        }
                    }
                    Err(message) => {
                        d.status = Status::Failed;
                        self.store.log(d.id, &message);
                        announce(d.id, "Download failed", format!("{}: {message}", d.name).into(), window, cx);
                    }
                }
            }
            let running = job.is_running();
            let queue_max = self.queues.iter().find(|q| q.id == d.queue).map_or(0, |q| q.max_concurrent as usize);
            let queue_running = in_queue.entry(d.queue).or_default();
            let room = active < max && (queue_max == 0 || *queue_running < queue_max);
            match d.status {
                Status::Downloading | Status::Queued if !running && room => {
                    job.start(Request {
                        id: d.id,
                        url: d.url.to_string(),
                        folder: expand_home(&d.folder),
                        selector: d.selector.clone(),
                        file_name: d.file_name.clone(),
                        ytdlp_bin: bin.clone(),
                        ffmpeg_bin: ffmpeg.clone(),
                        limit_bps,
                        fragments,
                    });
                    d.status = Status::Downloading;
                    active += 1;
                    *queue_running += 1;
                    changed = true;
                }
                Status::Downloading if !running => {
                    d.status = Status::Queued;
                    changed = true;
                }
                Status::Paused
                | Status::Failed
                | Status::Completed
                | Status::Missing
                | Status::Queued
                | Status::Later
                    if running =>
                {
                    job.stop();
                    active -= 1;
                    *queue_running = queue_running.saturating_sub(1);
                    changed = true;
                }
                Status::Completed | Status::Missing => {
                    let present = d.path.as_ref().is_none_or(|p| p.exists());
                    let status = if present { Status::Completed } else { Status::Missing };
                    changed |= d.status != status;
                    d.status = status;
                }
                _ => {}
            }
            if job.is_running() {
                d.received_bytes = job.received();
                d.bytes_per_sec = job.speed();
                d.record_speed();
                d.fragments = job.fragments();
                if let Some(total) = job.total() {
                    d.total_bytes = Some(total);
                }
                if d.source == Source::Direct
                    && let Some(name) = job.name()
                    && *d.name != *name
                {
                    d.kind = kind_for(&name);
                    d.name = name.into();
                }
                changed = true;
            }
        }
        if changed {
            self.sync_table(cx);
        }
        for queue in ended {
            if !crate::queue::pending(&self.downloads, queue) {
                self.queue_finished(queue, window, cx);
            }
        }
    }

    pub fn remove(&mut self, id: u64, cx: &mut Context<Self>) {
        self.remove_rows(&BTreeSet::from([id]), cx);
    }

    /// Drops the rows with their jobs and unfinished files; saved files stay.
    fn remove_rows(&mut self, ids: &BTreeSet<u64>, cx: &mut Context<Self>) {
        for &id in ids {
            crate::engine::discard(id, self.jobs.remove(&id));
        }
        self.downloads.retain(|d| !ids.contains(&d.id));
        self.checked.retain(|id| !ids.contains(id));
        self.sync_table(cx);
    }

    pub fn pause_all(&mut self, cx: &mut Context<Self>) {
        self.set_all(&[Status::Downloading, Status::Queued], Status::Paused, cx);
    }

    /// Leaves `Later` downloads alone: they wait for their queue.
    pub fn resume_all(&mut self, cx: &mut Context<Self>) {
        self.set_all(&[Status::Paused], Status::Downloading, cx);
    }

    fn set_all(&mut self, from: &[Status], to: Status, cx: &mut Context<Self>) {
        for d in self.downloads.iter_mut().filter(|d| from.contains(&d.status)) {
            d.status = to;
            d.bytes_per_sec = 0;
        }
        self.sync_table(cx);
    }

    pub fn new_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let auto_paste = AppSettings::get(cx).auto_paste;
        self.add.update(cx, |add, cx| {
            if auto_paste && add.url().read(cx).value().is_empty() {
                add.paste_copied_link(window, cx);
            }
            add.focus(window, cx);
            add.set_open(true, cx);
        })
    }

    /// Adds to the queue on screen, or Main; `later` adds it without starting.
    fn submit_add(&mut self, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.add.read(cx).request(self.next_id, cx) {
            Ok(mut download) => {
                download.position = self.next_position();
                download.queue = if let Filter::Queue(queue) = self.filter { queue } else { MAIN };
                if later {
                    download.status = Status::Later;
                }
                self.next_id += 1;
                self.checked = BTreeSet::from([download.id]);
                self.anchor = Some(download.id);
                self.downloads.insert(0, download);
                self.add.update(cx, |add, cx| add.reset(window, cx));
                self.sync_table(cx);
            }
            Err(message) => window.push_notification(message, cx),
        }
    }
}

/// A toast while the window is in front, otherwise the system notification center.
fn announce(id: u64, title: &'static str, body: SharedString, window: &mut Window, cx: &mut App) {
    if window.is_window_active() {
        window.push_notification(format!("{title}: {body}"), cx);
    } else {
        cx.show_system_notification(SystemNotification {
            tag: format!("download-{id}").into(),
            title: title.into(),
            body,
            actions: Vec::new(),
        });
    }
}

/// Quits, so no other open window keeps the process alive, unless downloads are running: those keep going
/// with the app hidden on macOS and minimized elsewhere.
fn close_main_window(active: bool, window: &mut Window, cx: &mut App) {
    if !active {
        cx.quit();
    } else if cfg!(target_os = "macos") {
        cx.hide();
    } else {
        window.minimize_window();
    }
}

/// Shows `path` selected in the system file manager.
#[cfg(not(windows))]
pub(crate) fn reveal(path: &Path) -> std::io::Result<std::process::Child> {
    if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        // shortcut: opens the folder without selecting the file; use the FileManager1 D-Bus call if that matters.
        std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn()
    }
}

#[cfg(windows)]
pub(crate) fn reveal(path: &Path) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    // Explorer wants the quotes after the comma, which Rust's own argument quoting won't produce.
    std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn()
}

pub(crate) fn expand_home(folder: &str) -> PathBuf {
    match folder.strip_prefix("~/") {
        Some(rest) => crate::ui::settings::home().join(rest),
        None => PathBuf::from(folder),
    }
}
