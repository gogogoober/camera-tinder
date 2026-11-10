//! Group photos by capture time: moments (minutes apart). `BURST_GAP_SECS` is the burst gap triage uses.

use chrono::NaiveDateTime;

use crate::model::Group;

/// Photos further apart than this start a new moment.
pub const MOMENT_GAP_SECS: i64 = 15 * 60;
/// Photos closer than this belong to the same burst.
pub const BURST_GAP_SECS: i64 = 3;

/// Split `times` (in capture order) wherever the gap exceeds `gap_secs`.
/// Photos with no time stay together in one group.
pub fn group_by_gap(times: &[Option<NaiveDateTime>], gap_secs: i64) -> Vec<Group> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=times.len() {
        let split = i == times.len()
            || match (times[i - 1], times[i]) {
                (Some(a), Some(b)) => (b - a).num_seconds().abs() > gap_secs,
                (None, None) => false,
                _ => true,
            };
        if split {
            out.push(Group { start, end: i });
            start = i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Option<NaiveDateTime> {
        Some(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap())
    }

    #[test]
    fn splits_moments() {
        let times = [
            t("2026-08-01T10:00:00"),
            t("2026-08-01T10:00:01"),
            t("2026-08-01T10:00:02"),
            t("2026-08-01T10:05:00"),
            t("2026-08-01T12:00:00"),
            None,
            None,
        ];
        let moments = group_by_gap(&times, MOMENT_GAP_SECS);
        assert_eq!(moments, vec![Group { start: 0, end: 4 }, Group { start: 4, end: 5 }, Group { start: 5, end: 7 }]);
    }

    #[test]
    fn empty_input() {
        assert!(group_by_gap(&[], 10).is_empty());
    }
}
