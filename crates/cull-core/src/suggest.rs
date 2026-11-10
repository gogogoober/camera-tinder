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

/// Row ids. Multi-day calendar rows are keyed by their days alone (events on the same days share
/// one row); single-day rows by day and title; gap rows by their days.
fn gap_id(a: &str, b: &str) -> String {
    format!("gap|{a}|{b}")
}
fn calendar_id(e: &CalendarEvent) -> String {
    if e.start_day == e.end_day { format!("cal|{}|{}|{}", e.start_day, e.end_day, e.title) } else { format!("cal|{}|{}", e.start_day, e.end_day) }
}

pub fn from_gaps(days: &[DaySummary], existing: &[Tag], dismissed: &[String]) -> Vec<TagSuggestion> {
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
            id: gap_id(&a.format(DAY_FMT).to_string(), &b.format(DAY_FMT).to_string()),
            name,
            start_day: a.format(DAY_FMT).to_string(),
            end_day: b.format(DAY_FMT).to_string(),
            source: TagSource::Gap,
            reason,
            photo_count: total,
            names: vec![],
            children: vec![],
        });
        i = j + 1;
    }
    out.retain(|s| !dismissed.contains(&s.id) && !existing.iter().any(|t| t.start_day == s.start_day && t.end_day == s.end_day));
    out
}

/// Calendar events as a two-level list for the Pick screen:
/// - multi-day events on exactly the same days share one row (`names`);
/// - an event whose days fall inside a multi-day row is that row's child (one level only: a
///   child never has children, so A ⊃ B ⊃ C gives A with B and C side by side);
/// - a child inside several overlapping rows is listed under each of them;
/// - everything else stays top-level.
///
/// `dismissed` row ids are left out *before* grouping, so dismissing a parent regroups what's left
/// (its children may land under another row).
pub fn from_calendar(days: &[DaySummary], events: &[CalendarEvent], existing: &[Tag], dismissed: &[String]) -> Vec<TagSuggestion> {
    let counts: Vec<(&str, u64)> = days.iter().filter_map(|d| Some((d.day.as_deref()?, d.stats.total))).collect();
    let mut rows: Vec<TagSuggestion> = Vec::new();
    for e in events {
        let photos: u64 = counts.iter().filter(|(d, _)| *d >= e.start_day.as_str() && *d <= e.end_day.as_str()).map(|(_, n)| n).sum();
        let id = calendar_id(e);
        if photos == 0 || dismissed.contains(&id) {
            continue;
        }
        let multi = e.start_day != e.end_day;
        if multi {
            if let Some(r) = rows.iter_mut().find(|r| r.start_day == e.start_day && r.end_day == e.end_day && r.source == TagSource::Calendar && !r.names.is_empty()) {
                if !r.names.contains(&e.title) {
                    r.names.push(e.title.clone());
                    r.name = r.names.join(" / ");
                }
                continue;
            }
        }
        rows.push(TagSuggestion {
            id,
            name: e.title.clone(),
            start_day: e.start_day.clone(),
            end_day: e.end_day.clone(),
            source: TagSource::Calendar,
            reason: format!("Calendar event, {photos} photos"),
            photo_count: photos,
            // Only multi-day rows may merge, so only they carry `names`.
            names: if multi { vec![e.title.clone()] } else { vec![] },
            children: vec![],
        });
    }
    rows.sort_by(|a, b| (&a.start_day, std::cmp::Reverse(&a.end_day), &a.name).cmp(&(&b.start_day, std::cmp::Reverse(&b.end_day), &b.name)));

    let inside = |c: &TagSuggestion, p: &TagSuggestion| {
        p.start_day != p.end_day && p.start_day <= c.start_day && c.end_day <= p.end_day && !(p.start_day == c.start_day && p.end_day == c.end_day)
    };
    // Roots: rows not inside any multi-day row. Everything else is a child of every root that holds it.
    let is_root: Vec<bool> = rows.iter().map(|c| !rows.iter().any(|p| inside(c, p))).collect();
    let mut roots: Vec<TagSuggestion> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if is_root[i] {
            let mut root = r.clone();
            root.children = rows.iter().enumerate().filter(|(j, c)| !is_root[*j] && inside(c, r)).map(|(_, c)| TagSuggestion { children: vec![], ..c.clone() }).collect();
            roots.push(root);
        }
    }

    // Drop rows already tagged. A tagged parent's children move up so they stay available.
    let tagged = |s: &TagSuggestion| existing.iter().any(|t| t.name == s.name && t.start_day == s.start_day);
    let mut out: Vec<TagSuggestion> = Vec::new();
    for mut r in roots {
        r.children.retain(|c| !tagged(c));
        if tagged(&r) {
            for c in std::mem::take(&mut r.children) {
                if !out.iter().any(|o| o.name == c.name && o.start_day == c.start_day && o.end_day == c.end_day) {
                    out.push(c);
                }
            }
        } else {
            out.push(r);
        }
    }
    out.sort_by(|a, b| (&a.start_day, &a.name).cmp(&(&b.start_day, &b.name)));
    out
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
        let s = from_gaps(&days, &[], &[]);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start_day.as_str(), s[0].end_day.as_str()), ("2026-08-01", "2026-08-03"));
        assert_eq!(s[0].photo_count, 270);
        assert_eq!(s[1].start_day, "2026-09-19");
    }

    #[test]
    fn skips_already_tagged_runs() {
        let days = [day("2026-08-01", 50), day("2026-08-02", 50)];
        let tag = Tag { id: 1, name: "Hike".into(), start_day: "2026-08-01".into(), end_day: "2026-08-02".into(), source: TagSource::Manual };
        assert!(from_gaps(&days, &[tag], &[]).is_empty());
    }

    #[test]
    fn calendar_events_need_photos() {
        let days = [day("2026-09-19", 260)];
        let ev = [
            CalendarEvent { title: "Maya's wedding".into(), start_day: "2026-09-19".into(), end_day: "2026-09-19".into() },
            CalendarEvent { title: "Dentist".into(), start_day: "2026-09-21".into(), end_day: "2026-09-21".into() },
        ];
        let s = from_calendar(&days, &ev, &[], &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Maya's wedding");
    }

    fn ev(t: &str, a: &str, b: &str) -> CalendarEvent {
        CalendarEvent { title: t.into(), start_day: a.into(), end_day: b.into() }
    }
    fn busy() -> Vec<DaySummary> {
        (1..=12).map(|d| day(&format!("2026-11-{d:02}"), 10)).collect()
    }

    #[test]
    fn nests_one_level_and_merges_same_days() {
        let e = [
            ev("Mom Visiting", "2026-11-01", "2026-11-10"),
            ev("Wedding", "2026-11-05", "2026-11-08"),
            ev("Hotel", "2026-11-05", "2026-11-08"),
            ev("Train", "2026-11-05", "2026-11-05"),
            ev("Lunch", "2026-11-08", "2026-11-08"),
            ev("Dinner", "2026-11-08", "2026-11-08"),
        ];
        let s = from_calendar(&busy(), &e, &[], &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Mom Visiting");
        let kids: Vec<&str> = s[0].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(kids, ["Wedding / Hotel", "Train", "Dinner", "Lunch"]);
        assert_eq!(s[0].children[0].names.len(), 2);
        assert!(s[0].children.iter().all(|c| c.children.is_empty()));
    }

    #[test]
    fn overlapping_parents_both_hold_the_child() {
        let e = [ev("A", "2026-11-05", "2026-11-08"), ev("B", "2026-11-06", "2026-11-09"), ev("Seven", "2026-11-07", "2026-11-07")];
        let s = from_calendar(&busy(), &e, &[], &[]);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|p| p.children.len() == 1 && p.children[0].name == "Seven"));
    }

    #[test]
    fn same_day_events_stay_separate_and_tagged_parent_promotes_children() {
        let e = [ev("One", "2026-11-03", "2026-11-03"), ev("Two", "2026-11-03", "2026-11-03"), ev("Trip", "2026-11-01", "2026-11-02"), ev("Walk", "2026-11-01", "2026-11-01")];
        let s = from_calendar(&busy(), &e, &[], &[]);
        assert_eq!(s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Trip", "One", "Two"]);
        let tag = Tag { id: 1, name: "Trip".into(), start_day: "2026-11-01".into(), end_day: "2026-11-02".into(), source: TagSource::Manual };
        let s = from_calendar(&busy(), &e, &[tag], &[]);
        assert_eq!(s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Walk", "One", "Two"]);
    }

    #[test]
    fn dismissing_a_parent_regroups_the_rest() {
        let e = [
            ev("Mom Visiting", "2026-11-01", "2026-11-10"),
            ev("Wedding", "2026-11-05", "2026-11-08"),
            ev("Train", "2026-11-05", "2026-11-05"),
        ];
        let s = from_calendar(&busy(), &e, &[], &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].id, "cal|2026-11-01|2026-11-10");
        let s = from_calendar(&busy(), &e, &[], &[s[0].id.clone()]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Wedding");
        assert_eq!(s[0].children[0].id, "cal|2026-11-05|2026-11-05|Train");
    }
}
