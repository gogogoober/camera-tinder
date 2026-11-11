//! What "a day" means for photos: it starts at 06:00, so a late night belongs to the evening before.
//! Used by culling (Pick calendar, day filters) and offload folders alike.

use chrono::{Duration, NaiveDate, NaiveDateTime};

/// Photos taken before this hour count as the previous day.
pub const DAY_START_HOUR: i64 = 6;

/// SQL expression for the day (`YYYY-MM-DD`, or NULL when undated) of `p.captured_at`.
/// Must agree with [`photo_day`]; `captured_at` is stored as `%Y-%m-%dT%H:%M:%S`.
pub const SQL_PHOTO_DAY: &str = "date(p.captured_at, '-6 hours')";

/// The day a photo taken at `t` belongs to.
pub fn photo_day(t: NaiveDateTime) -> NaiveDate {
    (t - Duration::hours(DAY_START_HOUR)).date()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    #[test]
    fn before_six_is_the_previous_day() {
        assert_eq!(photo_day(at("2026-10-04T01:30:00")).to_string(), "2026-10-03");
        assert_eq!(photo_day(at("2026-10-04T05:59:59")).to_string(), "2026-10-03");
        assert_eq!(photo_day(at("2026-10-04T06:00:00")).to_string(), "2026-10-04");
        assert_eq!(photo_day(at("2026-10-04T23:59:59")).to_string(), "2026-10-04");
        assert_eq!(photo_day(at("2027-01-01T02:00:00")).to_string(), "2026-12-31");
    }

    #[test]
    fn sql_agrees_with_rust() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for s in ["2026-10-04T01:30:00", "2026-10-04T06:00:00", "2027-01-01T05:00:00", "2026-03-01T03:00:00"] {
            let sql: String = conn
                .query_row(&format!("SELECT {}", SQL_PHOTO_DAY.replace("p.captured_at", "?1")), [s], |r| r.get(0))
                .unwrap();
            assert_eq!(sql, photo_day(at(s)).to_string(), "{s}");
        }
    }
}
