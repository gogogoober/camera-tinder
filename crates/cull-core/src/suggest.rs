//! Tag suggestions without AI.
//!
//! - **Calendar:** events from a platform `CalendarSource` that overlap days
//!   with photos become suggestions named after the event.
//!
//! No grouping is guessed from capture times: the only invented group is "All", which lives in the UI.
//! GPS place names and image-statistics hints (night, beach) plug in here later.

use crate::model::*;

/// A calendar entry reduced to the days it covers.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub start_day: String,
    pub end_day: String,
    /// An all-day event (or one spanning several days), not a timed one inside a day. Only those can name
    /// an offload folder: "Dinner 7–9 pm" must not claim a whole evening's photos.
    pub all_day: bool,
}

/// Row ids. Multi-day calendar rows are keyed by their days alone (events on the same days share
/// one row); single-day rows by day and title.
fn calendar_id(e: &CalendarEvent) -> String {
    if e.start_day == e.end_day { format!("cal|{}|{}|{}", e.start_day, e.end_day, e.title) } else { format!("cal|{}|{}", e.start_day, e.end_day) }
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
pub fn from_calendar(days: &[DaySummary], events: &[CalendarEvent], dismissed: &[String]) -> Vec<TagSuggestion> {
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

    let mut out = roots;
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
    fn calendar_events_need_photos() {
        let days = [day("2026-09-19", 260)];
        let ev = [
            CalendarEvent { title: "Maya's wedding".into(), start_day: "2026-09-19".into(), end_day: "2026-09-19".into(), all_day: true },
            CalendarEvent { title: "Dentist".into(), start_day: "2026-09-21".into(), end_day: "2026-09-21".into(), all_day: false },
        ];
        let s = from_calendar(&days, &ev, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Maya's wedding");
    }

    fn ev(t: &str, a: &str, b: &str) -> CalendarEvent {
        CalendarEvent { title: t.into(), start_day: a.into(), end_day: b.into(), all_day: true }
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
        let s = from_calendar(&busy(), &e, &[]);
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
        let s = from_calendar(&busy(), &e, &[]);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|p| p.children.len() == 1 && p.children[0].name == "Seven"));
    }

    #[test]
    fn same_day_events_stay_separate() {
        let e = [ev("One", "2026-11-03", "2026-11-03"), ev("Two", "2026-11-03", "2026-11-03"), ev("Trip", "2026-11-01", "2026-11-02"), ev("Walk", "2026-11-01", "2026-11-01")];
        let s = from_calendar(&busy(), &e, &[]);
        assert_eq!(s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Trip", "One", "Two"]);
        assert_eq!(s[0].children[0].name, "Walk");
    }

    #[test]
    fn dismissing_a_parent_regroups_the_rest() {
        let e = [
            ev("Mom Visiting", "2026-11-01", "2026-11-10"),
            ev("Wedding", "2026-11-05", "2026-11-08"),
            ev("Train", "2026-11-05", "2026-11-05"),
        ];
        let s = from_calendar(&busy(), &e, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].id, "cal|2026-11-01|2026-11-10");
        let s = from_calendar(&busy(), &e, &[s[0].id.clone()]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Wedding");
        assert_eq!(s[0].children[0].id, "cal|2026-11-05|2026-11-05|Train");
    }
}
