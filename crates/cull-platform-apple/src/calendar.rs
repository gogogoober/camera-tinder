//! `CalendarSource` backed by EventKit.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use cull_core::{CalendarEvent, CalendarSource, CoreError, Result};
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore};
use objc2_foundation::{NSDate, NSError};

use crate::Access;

/// EventKit predicates only look at ~4 years at a time.
const CHUNK_DAYS: u64 = 365 * 3;
/// Multi-week events (holidays, "out of office") are noise as tag suggestions.
const MAX_EVENT_DAYS: i64 = 21;

pub struct CalendarAccess;

impl CalendarAccess {
    pub fn status() -> Access {
        let s = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        match s {
            EKAuthorizationStatus::FullAccess => Access::Granted,
            EKAuthorizationStatus::NotDetermined => Access::NotDetermined,
            _ => Access::Denied,
        }
    }

    /// Ask macOS for read access. Blocks until the user answers (or 2 minutes pass).
    /// Never call from the main thread.
    pub fn request() -> Result<Access> {
        if Self::status() == Access::Granted {
            return Ok(Access::Granted);
        }
        let store = unsafe { EKEventStore::new() };
        let (tx, rx) = mpsc::channel::<bool>();
        let block = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
            let _ = tx.send(granted.as_bool());
        });
        unsafe { store.requestFullAccessToEventsWithCompletion(&*block as *const _ as *mut _) };
        rx.recv_timeout(Duration::from_secs(120)).map_err(|_| CoreError::Other("calendar permission prompt timed out".into()))?;
        Ok(Self::status())
    }
}

/// Reads events from every calendar the user has set up in Calendar.app.
pub struct EventKitCalendar {
    store: Retained<EKEventStore>,
}

// EKEventStore is documented as safe to use from multiple threads for reads.
unsafe impl Send for EventKitCalendar {}
unsafe impl Sync for EventKitCalendar {}

impl EventKitCalendar {
    /// Only succeeds when access was already granted (call `CalendarAccess::request` first).
    pub fn new() -> Option<Self> {
        (CalendarAccess::status() == Access::Granted).then(|| Self { store: unsafe { EKEventStore::new() } })
    }
}

fn local_midnight(d: NaiveDate) -> DateTime<Local> {
    Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap_or_else(|| Local.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()))
}

fn to_nsdate(t: DateTime<Local>) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSince1970(t.timestamp() as f64)
}

fn to_day(d: &NSDate) -> NaiveDate {
    let secs = d.timeIntervalSince1970() as i64;
    DateTime::from_timestamp(secs, 0).unwrap_or_default().with_timezone(&Local).date_naive()
}

impl CalendarSource for EventKitCalendar {
    fn events(&self, from: &str, to: &str) -> Result<Vec<CalendarEvent>> {
        let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| CoreError::Other(e.to_string()));
        let (from, to) = (parse(from)?, parse(to)?);
        let mut out: Vec<CalendarEvent> = Vec::new();

        let mut start = from;
        while start <= to {
            let end = (start + Days::new(CHUNK_DAYS)).min(to + Days::new(1));
            let predicate = unsafe {
                self.store.predicateForEventsWithStartDate_endDate_calendars(&to_nsdate(local_midnight(start)), &to_nsdate(local_midnight(end)), None)
            };
            let events = unsafe { self.store.eventsMatchingPredicate(&predicate) };
            for ev in events.iter() {
                if let Some(e) = convert(&ev) {
                    out.push(e);
                }
            }
            start = end;
        }

        // Recurring events show up once per occurrence; keep one per title+start day.
        out.sort_by(|a, b| (&a.start_day, &a.title).cmp(&(&b.start_day, &b.title)));
        out.dedup_by(|a, b| a.start_day == b.start_day && a.title == b.title);
        Ok(out)
    }
}

fn convert(ev: &EKEvent) -> Option<CalendarEvent> {
    let title = unsafe { ev.title() }.to_string();
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let (start_date, end_date) = unsafe { (ev.startDate(), ev.endDate()) };
    let start = to_day(&start_date);
    let mut end = to_day(&end_date);
    // Timed events ending exactly at midnight belong to the previous day.
    if end > start && !unsafe { ev.isAllDay() } {
        let end_secs = end_date.timeIntervalSince1970() as i64;
        let at_midnight = DateTime::from_timestamp(end_secs, 0).map(|t| t.with_timezone(&Local)).is_some_and(|t| t.time() == chrono::NaiveTime::MIN);
        if at_midnight {
            end = end.pred_opt().unwrap_or(end);
        }
    }
    if end < start || (end - start).num_days() >= MAX_EVENT_DAYS {
        return None;
    }
    Some(CalendarEvent { title: title.to_string(), start_day: start.to_string(), end_day: end.to_string() })
}
