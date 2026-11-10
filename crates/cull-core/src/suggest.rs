//! Tag suggestions without AI.
//!
//! - **Time gaps:** runs of consecutive days with photos become "trip" guesses.
//!   A day with far more photos than usual becomes a "busy day" guess.
//! - **Calendar:** events from a platform `CalendarSource` that overlap days
//!   with photos become suggestions named after the event.
//!
//! GPS place names and image-statistics hints (night, beach) plug in here later.

use chrono::{Datelike, Duration, NaiveDate};

use crate::model::*;

const DAY_FMT: &str = "%Y-%m-%d";
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn parse(day: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day, DAY_FMT).ok()
}

fn label(a: NaiveDate, b: NaiveDate) -> String {
    let f = |d: NaiveDate| format!("{} {}", MONTHS[d.month0() as usize], d.day());
    if a == b { f(a) } else { format!("{} – {}", f(a), f(b)) }
}

/// A calendar entry reduced to the days it covers.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub start_day: String,
    pub end_day: String,
}

pub fn from_gaps(days: &[DaySummary], existing: &[Tag]) -> Vec<TagSuggestion> {
    let mut dated: Vec<(NaiveDate, u64)> = days
        .iter()
        .filter_map(|d| Some((parse(d.day.as_deref()?)?, d.stats.total)))
        .filter(|(_, n)| *n > 0)
        .collect();
    dated.sort_by_key(|(d, _)| *d);
    if dated.is_empty() {
        return vec![];
    }

    let mut counts: Vec<u64> = dated.iter().map(|(_, n)| *n).collect();
    counts.sort_unstable();
    let median = counts[counts.len() / 2].max(1);

    let mut out = Vec::new();
    let mut i = 0;
    while i < dated.len() {
        let mut j = i;
        while j + 1 < dated.len() && dated[j + 1].0 - dated[j].0 == Duration::days(1) {
            j += 1;
        }
        let (a, b) = (dated[i].0, dated[j].0);
        let total: u64 = dated[i..=j].iter().map(|(_, n)| n).sum();
        let span = j - i + 1;
        let (name, reason) = if span >= 2 {
            (label(a, b), format!("Shot {span} days in a row, {total} photos"))
        } else if total >= median * 3 && total >= 30 {
            (label(a, b), format!("Busy day: {total} photos, about {}× your usual", total / median))
        } else {
            i = j + 1;
            continue;
        };
        out.push(TagSuggestion {
            name,
            start_day: a.format(DAY_FMT).to_string(),
            end_day: b.format(DAY_FMT).to_string(),
            source: TagSource::Gap,
            reason,
            photo_count: total,
        });
        i = j + 1;
    }
    out.retain(|s| !existing.iter().any(|t| t.start_day == s.start_day && t.end_day == s.end_day));
    out
}

pub fn from_calendar(days: &[DaySummary], events: &[CalendarEvent], existing: &[Tag]) -> Vec<TagSuggestion> {
    let counts: Vec<(&str, u64)> = days.iter().filter_map(|d| Some((d.day.as_deref()?, d.stats.total))).collect();
    events
        .iter()
        .filter_map(|e| {
            let photos: u64 = counts.iter().filter(|(d, _)| *d >= e.start_day.as_str() && *d <= e.end_day.as_str()).map(|(_, n)| n).sum();
            (photos > 0).then(|| TagSuggestion {
                name: e.title.clone(),
                start_day: e.start_day.clone(),
                end_day: e.end_day.clone(),
                source: TagSource::Calendar,
                reason: format!("Calendar event, {photos} photos"),
                photo_count: photos,
            })
        })
        .filter(|s| !existing.iter().any(|t| t.name == s.name && t.start_day == s.start_day))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: &str, n: u64) -> DaySummary {
        DaySummary { day: Some(d.into()), stats: Stats { total: n, undecided: n, ..Default::default() } }
    }

    #[test]
    fn finds_trips_and_busy_days() {
        let days = [
            day("2026-08-01", 120),
            day("2026-08-02", 90),
            day("2026-08-03", 60),
            day("2026-08-10", 8),
            day("2026-08-20", 12),
            day("2026-09-19", 300),
        ];
        let s = from_gaps(&days, &[]);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start_day.as_str(), s[0].end_day.as_str()), ("2026-08-01", "2026-08-03"));
        assert_eq!(s[0].photo_count, 270);
        assert_eq!(s[1].start_day, "2026-09-19");
    }

    #[test]
    fn skips_already_tagged_runs() {
        let days = [day("2026-08-01", 50), day("2026-08-02", 50)];
        let tag = Tag { id: 1, name: "Hike".into(), start_day: "2026-08-01".into(), end_day: "2026-08-02".into(), source: TagSource::Manual };
        assert!(from_gaps(&days, &[tag]).is_empty());
    }

    #[test]
    fn calendar_events_need_photos() {
        let days = [day("2026-09-19", 260)];
        let ev = [
            CalendarEvent { title: "Maya's wedding".into(), start_day: "2026-09-19".into(), end_day: "2026-09-19".into() },
            CalendarEvent { title: "Dentist".into(), start_day: "2026-09-21".into(), end_day: "2026-09-21".into() },
        ];
        let s = from_calendar(&days, &ev, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Maya's wedding");
    }
}
