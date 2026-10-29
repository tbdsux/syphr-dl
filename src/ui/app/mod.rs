use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    WindowExt,
    button::ButtonVariant,
    input::{InputEvent, InputState},
    menu::AppMenuBar,
    table::{TableEvent, TableState},
};
use gpui_kit::*;

use crate::download::{Download, Kind, Source, Status};
use crate::engine::{Job, Request};
use crate::ui::add_download::{AddDownload, AddEvent, kind_for};
use crate::ui::downloads_table::DownloadsTable;
use crate::ui::settings::AppSettings;

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
}

impl Filter {
    fn matches(self, d: &Download) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => matches!(d.status, Status::Downloading | Status::Paused | Status::Queued),
            Filter::Completed => matches!(d.status, Status::Completed | Status::Missing),
            Filter::Failed => d.status == Status::Failed,
            Filter::Kind(kind) => d.kind == kind,
        }
    }
}

pub struct SyphrApp {
    downloads: Vec<Download>,
    filter: Filter,
    /// Selected downloads by id; the details card shows when exactly one is selected.
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
            cx.subscribe_in(&add, window, |this, _, AddEvent::Submit, window, cx| this.submit_add(window, cx)),
            cx.observe(&add, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_table(cx);
                }
            }),
            cx.subscribe_in(&table, window, |this, table, event, window, cx| {
                if let TableEvent::SelectRow(ix) = event {
                    if this.own_row_events > 0 {
                        this.own_row_events -= 1;
                        return;
                    }
                    let ids: Vec<u64> = table.read(cx).delegate().rows.iter().map(|d| d.id).collect();
                    let modifiers = window.modifiers();
                    this.select_row(&ids, *ix, modifiers.shift, modifiers.secondary(cx), cx);
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
            allow_close(active, window, cx)
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut this = Self {
            next_id: 1,
            downloads: Vec::new(),
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

    fn has_active(&self) -> bool {
        self.downloads.iter().any(|d| matches!(d.status, Status::Downloading | Status::Queued))
    }

    /// Close Window and the Linux close button, which bypass the platform's should-close check.
    fn close(&mut self, window: &mut Window, cx: &mut App) {
        if allow_close(self.has_active(), window, cx) {
            window.remove_window();
        }
    }

    /// Pushes the filtered list into the table and keeps the selection on the same download.
    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_lowercase();
        let rows: Vec<Download> = self
            .downloads
            .iter()
            .filter(|d| self.filter.matches(d) && d.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let empty = if self.downloads.is_empty() {
            ("No downloads yet", "Add a file link or a YouTube URL to get started.")
        } else {
            ("Nothing here", "Try another category or clear the search.")
        };
        // Selection only covers what is on screen, so hidden rows are never acted on by accident.
        self.checked.retain(|id| rows.iter().any(|d| d.id == *id));
        let anchor_ix =
            self.anchor.filter(|id| self.checked.contains(id)).and_then(|id| rows.iter().position(|d| d.id == id));
        let checked = self.checked.clone();
        let mut own_events = 0;
        self.table.update(cx, |table, cx| {
            table.delegate_mut().rows = rows;
            table.delegate_mut().checked = checked;
            table.delegate_mut().empty = empty;
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
            table.refresh(cx);
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

    fn selected_download(&self) -> Option<&Download> {
        match self.checked.len() {
            1 => self.downloads.iter().find(|d| self.checked.contains(&d.id)),
            _ => None,
        }
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
            self.set_checked_status(&[Status::Paused], Status::Downloading, cx);
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
                        this.downloads.retain(|d| !checked.contains(&d.id));
                        this.sync_table(cx);
                    });
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
        // Dropping a job aborts it, which covers rows removed from the list.
        self.jobs.retain(|id, _| self.downloads.iter().any(|d| d.id == *id));
        let mut active = self.jobs.values().filter(|j| j.is_running()).count();
        let mut changed = false;
        // Oldest first, so the queue runs in the order downloads were added.
        for d in self.downloads.iter_mut().rev() {
            let job = self.jobs.entry(d.id).or_default();
            if let Some(result) = job.take_result() {
                changed = true;
                d.bytes_per_sec = 0;
                match result {
                    Ok(path) => {
                        d.status = Status::Completed;
                        d.received_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(d.received_bytes);
                        d.total_bytes = Some(d.received_bytes);
                        d.fragments.clear();
                        d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into();
                        d.path = Some(path);
                        if notify {
                            announce(d.id, "Download complete", d.name.clone(), window, cx);
                        }
                    }
                    Err(message) => {
                        d.status = Status::Failed;
                        announce(d.id, "Download failed", format!("{}: {message}", d.name).into(), window, cx);
                    }
                }
            }
            let running = job.is_running();
            match d.status {
                Status::Downloading | Status::Queued if !running && active < max => {
                    job.start(Request {
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
                    changed = true;
                }
                Status::Downloading if !running => {
                    d.status = Status::Queued;
                    changed = true;
                }
                Status::Paused | Status::Failed | Status::Completed | Status::Missing | Status::Queued if running => {
                    job.stop();
                    active -= 1;
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
    }

    pub fn remove(&mut self, id: u64, cx: &mut Context<Self>) {
        self.downloads.retain(|d| d.id != id);
        self.checked.remove(&id);
        self.sync_table(cx);
    }

    fn set_all(&mut self, from: &[Status], to: Status, cx: &mut Context<Self>) {
        for d in self.downloads.iter_mut().filter(|d| from.contains(&d.status)) {
            d.status = to;
            d.bytes_per_sec = 0;
        }
        self.sync_table(cx);
    }

    fn submit_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.add.read(cx).request(self.next_id, cx) {
            Ok(download) => {
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

/// Whether closing the main window may go ahead. On macOS the app hides instead, like other Mac apps,
/// so the list and running downloads survive; elsewhere running downloads keep it open, minimized.
pub fn allow_close(active: bool, window: &mut Window, cx: &mut App) -> bool {
    if cfg!(target_os = "macos") {
        cx.hide();
        false
    } else if active {
        window.minimize_window();
        false
    } else {
        true
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

fn expand_home(folder: &str) -> PathBuf {
    match folder.strip_prefix("~/") {
        Some(rest) => crate::ui::settings::home().join(rest),
        None => PathBuf::from(folder),
    }
}
