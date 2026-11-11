//! Folder names: which tag names a photo's folder, and names safe on every filesystem.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate};

/// A named set of days: a saved tag, or an all-day / multi-day calendar event.
#[derive(Debug, Clone)]
pub struct Span {
    pub name: String,
    pub days: BTreeSet<NaiveDate>,
    /// Saved tags win ties over calendar events.
    pub saved: bool,
}

impl Span {
    pub fn first(&self) -> NaiveDate {
        *self.days.first().expect("spans have days")
    }
}

/// The broadest span covering `day`: most days; ties → saved tag, then earliest start, then name.
pub fn broadest(spans: &[Span], day: NaiveDate) -> Option<&Span> {
    spans.iter().filter(|s| s.days.contains(&day)).max_by(|a, b| {
        (a.days.len(), a.saved, std::cmp::Reverse(a.first()), std::cmp::Reverse(&a.name))
            .cmp(&(b.days.len(), b.saved, std::cmp::Reverse(b.first()), std::cmp::Reverse(&b.name)))
    })
}

/// What a folder is for, inside `<brand>/<year>/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FolderKey {
    Trip(String),
    /// `YYYY-MM`
    Month(String),
    /// No capture time at all.
    Undated,
}

impl FolderKey {
    pub fn month_of(day: NaiveDate) -> Self {
        Self::Month(format!("{:04}-{:02}", day.year(), day.month()))
    }

    /// Stable key in the database (trip names case-insensitive).
    pub fn db_key(&self) -> String {
        match self {
            Self::Trip(n) => format!("trip:{}", n.to_lowercase()),
            Self::Month(m) => format!("month:{m}"),
            Self::Undated => "undated".into(),
        }
    }

    pub fn default_name(&self) -> String {
        match self {
            Self::Trip(n) => sanitize(n),
            Self::Month(m) => m.clone(),
            Self::Undated => "Undated".into(),
        }
    }
}

/// A folder name that works on APFS, exFAT and FAT: no `/ \ : * ? " < > |` or control characters, no
/// leading dot, no trailing dot or space, at most 100 characters. Empty → "Untitled".
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .collect();
    let mut s: String = cleaned.trim().trim_start_matches('.').chars().take(100).collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() { "Untitled".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn span(name: &str, from: &str, to: &str, saved: bool) -> Span {
        Span { name: name.into(), days: d(from).iter_days().take_while(|x| *x <= d(to)).collect(), saved }
    }

    #[test]
    fn the_broadest_tag_wins() {
        let spans = [
            span("Kyoto", "2026-10-05", "2026-10-07", true),
            span("Japan trip", "2026-10-01", "2026-10-14", false),
            span("Sushi class", "2026-10-06", "2026-10-06", true),
        ];
        assert_eq!(broadest(&spans, d("2026-10-06")).unwrap().name, "Japan trip");
        assert!(broadest(&spans, d("2026-10-20")).is_none());
        // Same length: the saved tag beats the calendar event.
        let tie = [span("Cal", "2026-10-01", "2026-10-02", false), span("Mine", "2026-10-01", "2026-10-02", true)];
        assert_eq!(broadest(&tie, d("2026-10-01")).unwrap().name, "Mine");
    }

    #[test]
    fn names_are_safe_everywhere() {
        assert_eq!(sanitize("Japan trip"), "Japan trip");
        assert_eq!(sanitize("Tokyo/Kyoto: day 1?"), "Tokyo-Kyoto- day 1-");
        assert_eq!(sanitize("..hidden"), "hidden");
        assert_eq!(sanitize("trailing. "), "trailing");
        assert_eq!(sanitize("   "), "Untitled");
        assert_eq!(sanitize(&"x".repeat(300)).len(), 100);
        assert_eq!(FolderKey::month_of(d("2026-11-03")).default_name(), "2026-11");
        assert_eq!(FolderKey::Trip("Japan Trip".into()).db_key(), "trip:japan trip");
    }
}
