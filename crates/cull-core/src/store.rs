//! SQLite persistence: the photo index and decisions.
//!
//! Everything is resumable: scans cache per-file fingerprints, and decisions
//! are stored per photo (by content fingerprint) plus an append-only log, so
//! history is kept even for Dislike/Hate (for a future "clean up rejects").

use std::path::Path;

use chrono::{Local, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::error::{CoreError, Result};
use crate::model::*;

const TIME_FMT: &str = "%Y-%m-%dT%H:%M:%S";

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS sources (
    id           TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    added_at     TEXT NOT NULL,
    last_scan    TEXT
);

CREATE TABLE IF NOT EXISTS photos (
    id          INTEGER PRIMARY KEY,
    fingerprint TEXT NOT NULL UNIQUE,
    source_id   TEXT NOT NULL,
    rel_path    TEXT NOT NULL,
    file_name   TEXT NOT NULL,
    kind        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    captured_at TEXT,
    camera      TEXT,
    last_seen   TEXT NOT NULL,
    sharpness   INTEGER
);
CREATE INDEX IF NOT EXISTS photos_source   ON photos(source_id);
CREATE INDEX IF NOT EXISTS photos_captured ON photos(captured_at);

-- Cache so rescans don't re-read unchanged files.
CREATE TABLE IF NOT EXISTS file_index (
    source_id TEXT NOT NULL,
    rel_path  TEXT NOT NULL,
    size      INTEGER NOT NULL,
    mtime     INTEGER NOT NULL,
    photo_id  INTEGER NOT NULL REFERENCES photos(id),
    PRIMARY KEY (source_id, rel_path)
);

-- Current decision per photo.
CREATE TABLE IF NOT EXISTS decisions (
    photo_id   INTEGER PRIMARY KEY REFERENCES photos(id),
    decision   TEXT NOT NULL,
    decided_at TEXT NOT NULL
);

-- Named runs of days ("Lisbon").
CREATE TABLE IF NOT EXISTS tags (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL,
    start_day TEXT NOT NULL,
    end_day   TEXT NOT NULL,
    source    TEXT NOT NULL
);

-- Full history (NULL decision = cleared via undo).
CREATE TABLE IF NOT EXISTS decision_log (
    id       INTEGER PRIMARY KEY,
    photo_id INTEGER NOT NULL REFERENCES photos(id),
    decision TEXT,
    previous TEXT,
    at       TEXT NOT NULL
);
"#;

pub struct NewPhoto<'a> {
    pub fingerprint: &'a str,
    pub source_id: &'a str,
    pub rel_path: &'a str,
    pub kind: MediaKind,
    pub size: u64,
    pub captured_at: Option<NaiveDateTime>,
    pub camera: Option<&'a str>,
    pub sharpness: Option<u8>,
}

pub struct Store {
    conn: Connection,
}

fn now() -> String {
    Local::now().naive_local().format(TIME_FMT).to_string()
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

const PHOTO_COLS: &str = "p.id, p.fingerprint, p.source_id, p.rel_path, p.file_name, p.kind, p.size, p.captured_at, p.camera, d.decision, p.sharpness";

fn row_to_photo(row: &Row) -> rusqlite::Result<Photo> {
    let kind: String = row.get(5)?;
    let captured: Option<String> = row.get(7)?;
    let decision: Option<String> = row.get(9)?;
    Ok(Photo {
        id: row.get(0)?,
        fingerprint: row.get(1)?,
        source_id: row.get(2)?,
        rel_path: row.get(3)?,
        file_name: row.get(4)?,
        kind: MediaKind::parse(&kind).unwrap_or(MediaKind::Jpeg),
        size: row.get::<_, i64>(6)? as u64,
        captured_at: captured.and_then(|s| NaiveDateTime::parse_from_str(&s, TIME_FMT).ok()),
        camera: row.get(8)?,
        decision: decision.as_deref().and_then(Decision::parse),
        sharpness: row.get::<_, Option<i64>>(10)?.map(|v| v.clamp(0, 100) as u8),
    })
}

/// SQL fragment counting decisions, used by stats and per-day summaries.
const STATS_COLS: &str = "COUNT(*),
    SUM(d.decision IS NULL),
    SUM(d.decision = 'like'),
    SUM(d.decision = 'dislike'),
    SUM(d.decision = 'favorite'),
    SUM(d.decision = 'hate')";

fn row_to_stats(row: &Row, offset: usize) -> rusqlite::Result<Stats> {
    let n = |i: usize| -> rusqlite::Result<u64> { Ok(row.get::<_, Option<i64>>(offset + i)?.unwrap_or(0) as u64) };
    Ok(Stats { total: n(0)?, undecided: n(1)?, like: n(2)?, dislike: n(3)?, favorite: n(4)?, hate: n(5)? })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        // Databases created before sharpness existed need the column added.
        let has_sharpness = conn.prepare("PRAGMA table_info(photos)")?.query_map([], |r| r.get::<_, String>(1))?.any(|c| c.is_ok_and(|c| c == "sharpness"));
        if !has_sharpness {
            conn.execute_batch("ALTER TABLE photos ADD COLUMN sharpness INTEGER")?;
        }
        Ok(Self { conn })
    }

    pub fn upsert_source(&self, id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sources (id, display_name, added_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name",
            params![id, name, now()],
        )?;
        Ok(())
    }

    /// Every source ever added: (id, display name).
    pub fn all_sources(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT id, display_name FROM sources ORDER BY display_name")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every source ever added (ids are platform-defined, e.g. canonical folder paths).
    pub fn all_source_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM sources")?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    pub fn mark_scanned(&self, source_id: &str) -> Result<()> {
        self.conn.execute("UPDATE sources SET last_scan = ?2 WHERE id = ?1", params![source_id, now()])?;
        Ok(())
    }

    pub fn source_photo_count(&self, source_id: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM photos WHERE source_id = ?1", [source_id], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Returns the photo id if this exact file (path, size, mtime) was indexed before.
    pub fn lookup_file(&self, source_id: &str, rel_path: &str, size: u64, mtime: i64) -> Result<Option<PhotoId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT photo_id FROM file_index WHERE source_id = ?1 AND rel_path = ?2 AND size = ?3 AND mtime = ?4",
                params![source_id, rel_path, size as i64, mtime],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Insert or refresh a photo. If the fingerprint is already known (e.g. the
    /// same shot seen on the SD card earlier), the existing id — and therefore
    /// its decision — is kept, and its location is updated.
    pub fn upsert_photo(&self, p: &NewPhoto) -> Result<(PhotoId, bool)> {
        let existed: Option<PhotoId> = self
            .conn
            .query_row("SELECT id FROM photos WHERE fingerprint = ?1", [p.fingerprint], |r| r.get(0))
            .optional()?;
        let file_name = p.rel_path.rsplit('/').next().unwrap_or(p.rel_path);
        let id: PhotoId = self.conn.query_row(
            "INSERT INTO photos (fingerprint, source_id, rel_path, file_name, kind, size, captured_at, camera, last_seen, sharpness)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(fingerprint) DO UPDATE SET
                source_id = excluded.source_id, rel_path = excluded.rel_path,
                file_name = excluded.file_name, last_seen = excluded.last_seen,
                sharpness = COALESCE(excluded.sharpness, photos.sharpness)
             RETURNING id",
            params![
                p.fingerprint,
                p.source_id,
                p.rel_path,
                file_name,
                p.kind.as_str(),
                p.size as i64,
                p.captured_at.map(|t| t.format(TIME_FMT).to_string()),
                p.camera,
                now(),
                p.sharpness.map(i64::from)
            ],
            |r| r.get(0),
        )?;
        Ok((id, existed.is_none()))
    }

    pub fn record_file(&self, source_id: &str, rel_path: &str, size: u64, mtime: i64, photo_id: PhotoId) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_index (source_id, rel_path, size, mtime, photo_id) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_id, rel_path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, photo_id = excluded.photo_id",
            params![source_id, rel_path, size as i64, mtime, photo_id],
        )?;
        Ok(())
    }

    /// Re-attach a previously indexed photo to this location (cheap rescan path).
    pub fn touch_photo(&self, id: PhotoId, source_id: &str, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE photos SET source_id = ?2, rel_path = ?3, last_seen = ?4 WHERE id = ?1",
            params![id, source_id, rel_path, now()],
        )?;
        Ok(())
    }

    pub fn transaction<T>(&mut self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn photo(&self, id: PhotoId) -> Result<Photo> {
        self.conn
            .query_row(
                &format!("SELECT {PHOTO_COLS} FROM photos p LEFT JOIN decisions d ON d.photo_id = p.id WHERE p.id = ?1"),
                [id],
                row_to_photo,
            )
            .optional()?
            .ok_or(CoreError::PhotoNotFound(id))
    }

    /// Photos in the given sources matching the filter, in capture order.
    pub fn query(&self, source_ids: &[String], filter: &Filter) -> Result<Vec<Photo>> {
        if source_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut sql = format!(
            "SELECT {PHOTO_COLS} FROM photos p LEFT JOIN decisions d ON d.photo_id = p.id WHERE p.source_id IN ({})",
            placeholders(source_ids.len())
        );
        let mut args: Vec<String> = source_ids.to_vec();
        match filter.status {
            StatusFilter::All => {}
            StatusFilter::Undecided => sql.push_str(" AND d.decision IS NULL"),
            StatusFilter::Keepers => sql.push_str(" AND d.decision IN ('like', 'favorite')"),
            StatusFilter::Rejects => sql.push_str(" AND d.decision IN ('dislike', 'hate')"),
        }
        match filter.day.as_deref() {
            None => {}
            Some("unknown") => sql.push_str(" AND p.captured_at IS NULL"),
            Some(day) => match filter.day_to.as_deref() {
                Some(to) if to != day => {
                    sql.push_str(" AND substr(p.captured_at, 1, 10) BETWEEN ? AND ?");
                    args.push(day.to_string());
                    args.push(to.to_string());
                }
                _ => {
                    sql.push_str(" AND substr(p.captured_at, 1, 10) = ?");
                    args.push(day.to_string());
                }
            },
        }
        sql.push_str(" ORDER BY p.captured_at IS NULL, p.captured_at, p.file_name");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), row_to_photo)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self, source_ids: &[String]) -> Result<Stats> {
        if source_ids.is_empty() {
            return Ok(Stats::default());
        }
        let sql = format!(
            "SELECT {STATS_COLS} FROM photos p LEFT JOIN decisions d ON d.photo_id = p.id WHERE p.source_id IN ({})",
            placeholders(source_ids.len())
        );
        Ok(self.conn.query_row(&sql, params_from_iter(source_ids.iter()), |r| row_to_stats(r, 0))?)
    }

    pub fn days(&self, source_ids: &[String]) -> Result<Vec<DaySummary>> {
        if source_ids.is_empty() {
            return Ok(vec![]);
        }
        let sql = format!(
            "SELECT substr(p.captured_at, 1, 10) AS day, {STATS_COLS}
             FROM photos p LEFT JOIN decisions d ON d.photo_id = p.id
             WHERE p.source_id IN ({})
             GROUP BY day ORDER BY day IS NULL, day",
            placeholders(source_ids.len())
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(source_ids.iter()), |r| {
            Ok(DaySummary { day: r.get(0)?, stats: row_to_stats(r, 1)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn decision(&self, id: PhotoId) -> Result<Option<Decision>> {
        let d: Option<String> = self
            .conn
            .query_row("SELECT decision FROM decisions WHERE photo_id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(d.as_deref().and_then(Decision::parse))
    }

    fn apply_decision(&self, id: PhotoId, decision: Option<Decision>, at: &str) -> Result<Option<Decision>> {
        let previous = self.decision(id)?;
        match decision {
            Some(d) => self.conn.execute(
                "INSERT INTO decisions (photo_id, decision, decided_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(photo_id) DO UPDATE SET decision = excluded.decision, decided_at = excluded.decided_at",
                params![id, d.as_str(), at],
            )?,
            None => self.conn.execute("DELETE FROM decisions WHERE photo_id = ?1", [id])?,
        };
        self.conn.execute(
            "INSERT INTO decision_log (photo_id, decision, previous, at) VALUES (?1, ?2, ?3, ?4)",
            params![id, decision.map(Decision::as_str), previous.map(Decision::as_str), at],
        )?;
        Ok(previous)
    }

    /// Set (or clear, with `None`) a decision. Returns the previous one.
    pub fn set_decision(&mut self, id: PhotoId, decision: Option<Decision>) -> Result<Option<Decision>> {
        Ok(self.set_many(&[(id, decision)])?.remove(0))
    }

    /// Apply many decisions in one transaction. Returns each photo's previous decision, in order.
    pub fn set_many(&mut self, items: &[(PhotoId, Option<Decision>)]) -> Result<Vec<Option<Decision>>> {
        self.transaction(|s| {
            let at = now();
            items.iter().map(|(id, d)| s.apply_decision(*id, *d, &at)).collect()
        })
    }

    pub fn set_photo_sharpness(&self, id: PhotoId, score: u8) -> Result<()> {
        self.conn.execute("UPDATE photos SET sharpness = ?2 WHERE id = ?1", params![id, i64::from(score)])?;
        Ok(())
    }

    pub fn needs_sharpness(&self, id: PhotoId) -> Result<bool> {
        Ok(self.conn.query_row("SELECT sharpness IS NULL FROM photos WHERE id = ?1", [id], |r| r.get(0))?)
    }

    // ---- tags ---------------------------------------------------------

    pub fn tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self.conn.prepare("SELECT id, name, start_day, end_day, source FROM tags ORDER BY start_day, end_day, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                start_day: r.get(2)?,
                end_day: r.get(3)?,
                source: TagSource::parse(&r.get::<_, String>(4)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_tag(&self, name: &str, start_day: &str, end_day: &str, source: TagSource) -> Result<Tag> {
        let id: i64 = self.conn.query_row(
            "INSERT INTO tags (name, start_day, end_day, source) VALUES (?1, ?2, ?3, ?4) RETURNING id",
            params![name, start_day, end_day, source.as_str()],
            |r| r.get(0),
        )?;
        Ok(Tag { id, name: name.into(), start_day: start_day.into(), end_day: end_day.into(), source })
    }

    pub fn remove_tag(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM tags WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(store: &Store, fp: &str, src: &str, path: &str, when: Option<&str>) -> PhotoId {
        store
            .upsert_photo(&NewPhoto {
                fingerprint: fp,
                source_id: src,
                rel_path: path,
                kind: MediaKind::Jpeg,
                size: 10,
                captured_at: when.map(|w| NaiveDateTime::parse_from_str(w, TIME_FMT).unwrap()),
                camera: Some("NIKON Z 6"),
                sharpness: None,
            })
            .unwrap()
            .0
    }

    #[test]
    fn decisions_follow_content_not_path() {
        let mut s = Store::open_in_memory().unwrap();
        let id = photo(&s, "fp1", "sdcard", "DCIM/DSC_0001.JPG", Some("2026-07-01T10:00:00"));
        s.set_decision(id, Some(Decision::Favorite)).unwrap();

        // Same file later appears on the laptop in a different folder.
        let id2 = photo(&s, "fp1", "laptop", "Trips/Lisbon/DSC_0001.JPG", Some("2026-07-01T10:00:00"));
        assert_eq!(id, id2);
        let p = s.photo(id).unwrap();
        assert_eq!(p.decision, Some(Decision::Favorite));
        assert_eq!(p.source_id, "laptop");
    }

    #[test]
    fn filters_stats_days_and_undo() {
        let mut s = Store::open_in_memory().unwrap();
        let src = vec!["a".to_string()];
        let a = photo(&s, "1", "a", "x/1.jpg", Some("2026-07-01T10:00:00"));
        let b = photo(&s, "2", "a", "x/2.jpg", Some("2026-07-01T09:00:00"));
        let c = photo(&s, "3", "a", "x/3.jpg", Some("2026-07-02T09:00:00"));
        let _d = photo(&s, "4", "a", "x/4.jpg", None);

        // Ordered by capture time, unknown last.
        let all = s.query(&src, &Filter { day: None, day_to: None, status: StatusFilter::All }).unwrap();
        assert_eq!(all.iter().map(|p| p.file_name.as_str()).collect::<Vec<_>>(), ["2.jpg", "1.jpg", "3.jpg", "4.jpg"]);

        s.set_decision(a, Some(Decision::Like)).unwrap();
        s.set_decision(b, Some(Decision::Hate)).unwrap();
        s.set_decision(c, Some(Decision::Favorite)).unwrap();

        let undecided = s.query(&src, &Filter::default()).unwrap();
        assert_eq!(undecided.len(), 1);
        let keepers = s.query(&src, &Filter { day: None, day_to: None, status: StatusFilter::Keepers }).unwrap();
        assert_eq!(keepers.len(), 2);
        let day1 = s.query(&src, &Filter { day: Some("2026-07-01".into()), day_to: None, status: StatusFilter::All }).unwrap();
        assert_eq!(day1.len(), 2);

        let st = s.stats(&src).unwrap();
        assert_eq!(st, Stats { total: 4, undecided: 1, like: 1, dislike: 0, favorite: 1, hate: 1 });

        let days = s.days(&src).unwrap();
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].day.as_deref(), Some("2026-07-01"));
        assert_eq!(days[2].day, None);

        // Day ranges.
        let range = s.query(&src, &Filter { day: Some("2026-07-01".into()), day_to: Some("2026-07-02".into()), status: StatusFilter::All }).unwrap();
        assert_eq!(range.len(), 3);

        // Undo = restore previous.
        let prev = s.set_decision(c, None).unwrap();
        assert_eq!(prev, Some(Decision::Favorite));
        assert_eq!(s.decision(c).unwrap(), None);
    }

    #[test]
    fn bulk_decisions_and_tags() {
        let mut s = Store::open_in_memory().unwrap();
        let a = photo(&s, "1", "a", "x/1.jpg", Some("2026-07-01T10:00:00"));
        let b = photo(&s, "2", "a", "x/2.jpg", Some("2026-07-01T10:00:01"));
        s.set_decision(a, Some(Decision::Like)).unwrap();
        let prev = s.set_many(&[(a, Some(Decision::Dislike)), (b, Some(Decision::Dislike))]).unwrap();
        assert_eq!(prev, vec![Some(Decision::Like), None]);
        assert_eq!(s.decision(b).unwrap(), Some(Decision::Dislike));

        let t = s.add_tag("Hike", "2026-07-01", "2026-07-03", TagSource::Manual).unwrap();
        assert_eq!(s.tags().unwrap().len(), 1);
        s.remove_tag(t.id).unwrap();
        assert!(s.tags().unwrap().is_empty());

        s.set_photo_sharpness(a, 12).unwrap();
        assert_eq!(s.photo(a).unwrap().sharpness, Some(12));
        assert!(!s.needs_sharpness(a).unwrap());
        assert!(s.needs_sharpness(b).unwrap());
    }
}
