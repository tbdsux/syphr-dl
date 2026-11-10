//! Named download queues: their schedule, their order, and what happens when one finishes.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};

use crate::download::{Download, Status};

/// The queue every download lands in unless the user picks another. It can't be deleted.
pub const MAIN: u64 = 1;
/// Every day of the week, Monday in the lowest bit.
pub const EVERY_DAY: u8 = 0x7f;

#[derive(Clone, Debug, PartialEq)]
pub struct Queue {
    pub id: u64,
    pub name: String,
    /// When the queue's waiting downloads start; None never starts them on its own.
    pub start: Option<NaiveTime>,
    /// When its running downloads stop again; before `start` means the next day.
    pub stop: Option<NaiveTime>,
    /// Days the schedule runs on, Monday in the lowest bit; ignored when `date` is set.
    pub days: u8,
    /// Runs only on this date.
    pub date: Option<NaiveDate>,
    /// Downloads of this queue running at once; 0 uses the global limit.
    pub max_concurrent: u32,
    pub on_finish: Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Nothing,
    Quit,
    Sleep,
    ShutDown,
}

/// Where a reordered download goes within its queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

impl Queue {
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            start: None,
            stop: None,
            days: EVERY_DAY,
            date: None,
            max_concurrent: 0,
            on_finish: Finish::Nothing,
        }
    }

    pub fn scheduled(&self) -> bool {
        self.start.is_some() || self.stop.is_some()
    }

    fn runs_on(&self, date: NaiveDate) -> bool {
        match self.date {
            Some(once) => once == date,
            None => self.days & (1 << date.weekday().num_days_from_monday()) != 0,
        }
    }

    /// Whether `now` falls between the start and stop times on a scheduled day. Without a stop time the
    /// window is the start minute alone, so it starts the queue once and never stops it; without a start
    /// time it runs from midnight to the stop time.
    pub fn inside(&self, now: NaiveDateTime) -> bool {
        if !self.scheduled() {
            return false;
        }
        let from = self.start.unwrap_or(NaiveTime::MIN);
        let until = self.stop.unwrap_or_else(|| from.overflowing_add_signed(Duration::minutes(1)).0);
        let (today, t) = (now.date(), now.time());
        if from < until {
            self.runs_on(today) && from <= t && t < until
        } else {
            // Past midnight: the window opened on the day before.
            (self.runs_on(today) && t >= from) || (today.pred_opt().is_some_and(|d| self.runs_on(d)) && t < until)
        }
    }

    /// The next time after `now` the queue starts on its own.
    pub fn next_start(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let start = self.start?;
        (0..=7)
            .filter_map(|days| now.date().checked_add_signed(Duration::days(days)))
            .filter(|&date| self.runs_on(date))
            .map(|date| date.and_time(start))
            .find(|&at| at > now)
    }
}

impl Finish {
    pub const ALL: [Finish; 4] = [Finish::Nothing, Finish::Quit, Finish::Sleep, Finish::ShutDown];

    pub fn key(self) -> &'static str {
        match self {
            Finish::Nothing => "nothing",
            Finish::Quit => "quit",
            Finish::Sleep => "sleep",
            Finish::ShutDown => "shutdown",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL.into_iter().find(|f| f.key() == key).unwrap_or(Finish::Nothing)
    }

    pub fn label(self) -> &'static str {
        match self {
            Finish::Nothing => "Do nothing",
            Finish::Quit => "Quit Syphr",
            Finish::Sleep => "Sleep the computer",
            Finish::ShutDown => "Shut down the computer",
        }
    }

    /// Puts the computer to sleep or shuts it down. Quitting is the app's job.
    pub fn run_system(self) -> std::io::Result<()> {
        use std::process::Command;
        let (program, args): (&str, &[&str]) = match self {
            Finish::Nothing | Finish::Quit => return Ok(()),
            Finish::Sleep if cfg!(target_os = "macos") => ("pmset", &["sleepnow"]),
            Finish::Sleep if cfg!(windows) => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
            Finish::Sleep => ("systemctl", &["suspend"]),
            Finish::ShutDown if cfg!(target_os = "macos") => {
                ("osascript", &["-e", "tell application \"System Events\" to shut down"])
            }
            Finish::ShutDown if cfg!(windows) => ("shutdown", &["/s", "/t", "0"]),
            Finish::ShutDown => ("systemctl", &["poweroff"]),
        };
        let status = Command::new(program).args(args).status()?;
        if status.success() { Ok(()) } else { Err(std::io::Error::other(format!("{program} exited with {status}"))) }
    }
}

/// Moves download `id` within its queue by swapping places with its neighbour, or past every other one.
pub fn reorder(downloads: &mut [Download], id: u64, to: Move) {
    let Some(me) = downloads.iter().find(|d| d.id == id).map(|d| (d.queue, d.position, d.id)) else { return };
    let (queue, key) = (me.0, (me.1, me.2));
    let peers = downloads.iter().filter(|d| d.queue == queue && d.id != id).map(|d| ((d.position, d.id), d.id));
    let target = match to {
        Move::Up => peers.filter(|(k, _)| *k < key).max(),
        Move::Down => peers.filter(|(k, _)| *k > key).min(),
        Move::Top => peers.min().filter(|(k, _)| *k < key).map(|(k, _)| ((k.0 - 1, 0), 0)),
        Move::Bottom => peers.max().filter(|(k, _)| *k > key).map(|(k, _)| ((k.0 + 1, 0), 0)),
    };
    let Some(((position, _), other)) = target else { return };
    if matches!(to, Move::Up | Move::Down)
        && let Some(o) = downloads.iter_mut().find(|d| d.id == other)
    {
        o.position = key.0;
    }
    if let Some(d) = downloads.iter_mut().find(|d| d.id == id) {
        d.position = position;
    }
}

/// Indexes into `downloads` in the order the pump should consider them: by position, then oldest first.
pub fn start_order(downloads: &[Download]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..downloads.len()).collect();
    order.sort_by_key(|&i| (downloads[i].position, downloads[i].id));
    order
}

/// Whether a queue still has work that will run, now or once it starts.
pub fn pending(downloads: &[Download], queue: u64) -> bool {
    downloads
        .iter()
        .any(|d| d.queue == queue && matches!(d.status, Status::Downloading | Status::Queued | Status::Later))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{Kind, Source};

    fn at(date: &str, time: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").unwrap()
    }

    fn time(t: &str) -> Option<NaiveTime> {
        Some(NaiveTime::parse_from_str(t, "%H:%M").unwrap())
    }

    // 2026-11-09 is a Monday.
    const MON: &str = "2026-11-09";
    const TUE: &str = "2026-11-10";
    const SAT: &str = "2026-11-14";

    #[test]
    fn window_on_chosen_days() {
        let q = Queue { start: time("09:00"), stop: time("17:00"), days: 0b0011111, ..Queue::new(2, "Work") };
        assert!(q.inside(at(MON, "09:00")));
        assert!(q.inside(at(TUE, "16:59")));
        assert!(!q.inside(at(TUE, "17:00")));
        assert!(!q.inside(at(TUE, "08:59")));
        assert!(!q.inside(at(SAT, "12:00")));
        assert!(!Queue::new(1, "Main").inside(at(MON, "12:00")));
    }

    #[test]
    fn window_past_midnight_belongs_to_the_start_day() {
        // Fridays only, 23:00 to 06:00.
        let q = Queue { start: time("23:00"), stop: time("06:00"), days: 1 << 4, ..Queue::new(2, "Night") };
        assert!(q.inside(at("2026-11-13", "23:30")));
        assert!(q.inside(at(SAT, "05:59")));
        assert!(!q.inside(at(SAT, "06:00")));
        assert!(!q.inside(at(SAT, "23:30")));
        assert!(!q.inside(at("2026-11-13", "05:00")));
    }

    #[test]
    fn window_without_stop_or_start() {
        let start_only = Queue { start: time("02:00"), ..Queue::new(2, "Start") };
        assert!(start_only.inside(at(MON, "02:00")));
        assert!(!start_only.inside(at(MON, "02:01")));
        let late = Queue { start: time("23:59"), ..Queue::new(2, "Late") };
        assert!(late.inside(at(MON, "23:59")));
        assert!(!late.inside(at(TUE, "00:00")));
        let stop_only = Queue { stop: time("08:00"), ..Queue::new(2, "Stop") };
        assert!(stop_only.inside(at(MON, "07:59")));
        assert!(!stop_only.inside(at(MON, "08:00")));
    }

    #[test]
    fn once_on_a_date() {
        let q = Queue {
            start: time("22:00"),
            stop: time("01:00"),
            date: NaiveDate::from_ymd_opt(2026, 11, 10),
            ..Queue::new(2, "Once")
        };
        assert!(q.inside(at(TUE, "22:30")));
        assert!(q.inside(at("2026-11-11", "00:30")));
        assert!(!q.inside(at("2026-11-17", "22:30")));
        assert_eq!(q.next_start(at(MON, "12:00")), Some(at(TUE, "22:00")));
        assert_eq!(q.next_start(at(TUE, "22:00")), None);
    }

    #[test]
    fn next_start_skips_to_the_next_scheduled_day() {
        let weekdays = Queue { start: time("09:00"), days: 0b0011111, ..Queue::new(2, "Work") };
        assert_eq!(weekdays.next_start(at(MON, "08:00")), Some(at(MON, "09:00")));
        assert_eq!(weekdays.next_start(at(MON, "09:00")), Some(at(TUE, "09:00")));
        assert_eq!(weekdays.next_start(at("2026-11-13", "10:00")), Some(at("2026-11-16", "09:00")));
        let one_day = Queue { days: 1, ..weekdays.clone() };
        assert_eq!(one_day.next_start(at(MON, "10:00")), Some(at("2026-11-16", "09:00")));
        assert_eq!(Queue { days: 0, ..weekdays.clone() }.next_start(at(MON, "08:00")), None);
        assert_eq!(Queue::new(1, "Main").next_start(at(MON, "08:00")), None);
    }

    fn download(id: u64, queue: u64, position: i64) -> Download {
        Download {
            id,
            name: "".into(),
            url: "".into(),
            kind: Kind::Archive,
            source: Source::Direct,
            status: Status::Queued,
            total_bytes: None,
            received_bytes: 0,
            bytes_per_sec: 0,
            folder: "".into(),
            format: None,
            selector: None,
            file_name: None,
            path: None,
            fragments: Vec::new(),
            speed_history: Vec::new(),
            queue,
            position,
        }
    }

    fn order(list: &[Download], queue: u64) -> Vec<u64> {
        start_order(list).into_iter().map(|i| &list[i]).filter(|d| d.queue == queue).map(|d| d.id).collect()
    }

    #[test]
    fn reorders_within_a_queue() {
        // Newest first, like the app keeps them; queue 2 sits between queue 1's positions.
        let mut list =
            vec![download(5, 1, 5), download(4, 2, 4), download(3, 1, 3), download(2, 1, 2), download(1, 1, 1)];
        assert_eq!(order(&list, 1), [1, 2, 3, 5]);
        reorder(&mut list, 5, Move::Up);
        assert_eq!(order(&list, 1), [1, 2, 5, 3]);
        reorder(&mut list, 1, Move::Down);
        assert_eq!(order(&list, 1), [2, 1, 5, 3]);
        reorder(&mut list, 3, Move::Top);
        assert_eq!(order(&list, 1), [3, 2, 1, 5]);
        reorder(&mut list, 3, Move::Bottom);
        assert_eq!(order(&list, 1), [2, 1, 5, 3]);
        // Already at the edge, and alone in its queue: nothing moves.
        reorder(&mut list, 2, Move::Up);
        reorder(&mut list, 2, Move::Top);
        reorder(&mut list, 4, Move::Down);
        assert_eq!(order(&list, 1), [2, 1, 5, 3]);
        assert_eq!(order(&list, 2), [4]);
        assert_eq!(list.iter().find(|d| d.id == 4).unwrap().position, 4);
    }

    #[test]
    fn pending_counts_waiting_work() {
        let mut list = vec![download(1, 2, 1)];
        assert!(pending(&list, 2));
        list[0].status = Status::Later;
        assert!(pending(&list, 2));
        list[0].status = Status::Completed;
        assert!(!pending(&list, 2));
        assert!(!pending(&list, 1));
        assert_eq!(Finish::from_key(Finish::Sleep.key()), Finish::Sleep);
        assert_eq!(Finish::from_key("bogus"), Finish::Nothing);
    }
}
