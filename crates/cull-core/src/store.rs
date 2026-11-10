//! SQLite persistence: the photo index and decisions.
//!
//! Everything is resumable: scans cache per-file fingerprints, and decisions
//! are stored per photo (by content fingerprint) plus an append-only log, so
//! history is kept even for Dislike/Hate (for a future "clean up rejects").

use std::collections::HashMap;
use std::path::Path;

use chrono::{Local, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::analysis::pipeline::StepResult;
use crate::analysis::{AnalysisKind, PhotoAnalysis};
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::triage::OutcomeEntry;

const TIME_FMT: &str = "%Y-%m-%dT%H:%M:%S";

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS sources (
    id           TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    added_at     TEXT NOT NULL,
    last_scan    TEXT -- unused (never read); kept so the schema matches existing databases
    -- Databases from Nov 2026 builds may also have unused scan_* columns (a removed folder-skip cache).
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
    -- Unused: the old scan-time blur score. Kept so existing databases need no migration.
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

-- Older databases also have a `tags` table (saved day ranges, removed Nov 2026). Unused; left alone.

-- Saved tags: a name for a set of days. (Not the old unused `tags` table.) Days may be in many tags.
CREATE TABLE IF NOT EXISTS day_tags (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS day_tag_days (
    tag_id INTEGER NOT NULL REFERENCES day_tags(id) ON DELETE CASCADE,
    day    TEXT NOT NULL,
    PRIMARY KEY (tag_id, day)
);

-- Every export of a photo (append-only): so "already exported" can be a filter. Never deletes files.
CREATE TABLE IF NOT EXISTS exports (
    id          INTEGER PRIMARY KEY,
    photo_id    INTEGER NOT NULL REFERENCES photos(id),
    destination TEXT NOT NULL,
    exported_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS exports_photo ON exports(photo_id);

-- Photo analysis results, one row per photo and kind (see analysis/mod.rs).
-- `implementation` names the engine; when it changes the row is stale and recomputed.
CREATE TABLE IF NOT EXISTS analysis (
    photo_id       INTEGER NOT NULL REFERENCES photos(id),
    kind           TEXT NOT NULL,
    implementation TEXT NOT NULL,
    result         TEXT,
    error          TEXT,
    computed_at    TEXT NOT NULL,
    PRIMARY KEY (photo_id, kind)
);

-- Full history (NULL decision = cleared via undo).
CREATE TABLE IF NOT EXISTS decision_log (
    id       INTEGER PRIMARY KEY,
    photo_id INTEGER NOT NULL REFERENCES photos(id),
    decision TEXT,
    previous TEXT,
    at       TEXT NOT NULL
);

-- What triage suggested vs what the user decided (append-only; an undo is not logged).
CREATE TABLE IF NOT EXISTS outcomes (
    id           INTEGER PRIMARY KEY,
    photo_id     INTEGER NOT NULL REFERENCES photos(id),
    plan_version TEXT NOT NULL,
    question     TEXT NOT NULL,
    place        TEXT NOT NULL,
    suggestion   TEXT NOT NULL, -- the TriageItem shown, as JSON
    decision     TEXT NOT NULL,
    at           TEXT NOT NULL
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
}

pub struct Store {
    conn: Connection,
}

fn now() -> String {
    Local::now().naive_local().format(TIME_FMT).to_string()
}

/// SQL condition (on `p.captured_at`) for "taken on one of `days`", with its arguments.
/// `"unknown"` = no capture date; an empty list matches nothing. Shared by `query` and
/// `photo_ids_on_days`, so a reset covers exactly what the triage screen shows for those days.
fn days_condition(days: &[String]) -> (String, Vec<String>) {
    let dated: Vec<String> = days.iter().filter(|d| *d != "unknown").cloned().collect();
    let mut any = Vec::new();
    if !dated.is_empty() {
        any.push(format!("substr(p.captured_at, 1, 10) IN ({})", placeholders(dated.len())));
    }
    if days.iter().any(|d| d == "unknown") {
        any.push("p.captured_at IS NULL".to_string());
    }
    if any.is_empty() { ("0".to_string(), Vec::new()) } else { (format!("({})", any.join(" OR ")), dated) }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

const PHOTO_COLS: &str = "p.id, p.fingerprint, p.source_id, p.rel_path, p.file_name, p.kind, p.size, p.captured_at, p.camera, d.decision";

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
        // Another copy of the app (e.g. a dev build) may be writing: wait for it instead of failing.
        // (rusqlite already defaults to 5 s; this states it, longer, for a big scan batch.)
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        // Databases created before these columns existed need them added.
        let columns = |table: &str| -> Result<Vec<String>> {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let names = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(names)
        };
        if !columns("photos")?.iter().any(|c| c == "sharpness") {
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
            "INSERT INTO photos (fingerprint, source_id, rel_path, file_name, kind, size, captured_at, camera, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(fingerprint) DO UPDATE SET
                source_id = excluded.source_id, rel_path = excluded.rel_path,
                file_name = excluded.file_name, last_seen = excluded.last_seen
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
                now()
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

    /// Forget photos of `source_id` whose file is no longer in `listed` (a complete listing of that source
    /// just taken) and that can't be shown or exported anyway: no cached preview (`has_master`) and never
    /// exported. Database only; files are never touched. Returns how many were removed.
    pub fn forget_vanished(&self, source_id: &str, listed: &std::collections::HashSet<&str>, has_master: &dyn Fn(&str) -> bool) -> Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT id, rel_path, fingerprint FROM photos WHERE source_id = ?1 AND id NOT IN (SELECT photo_id FROM exports)",
        )?;
        let gone: Vec<PhotoId> = stmt
            .query_map([source_id], |r| Ok((r.get::<_, PhotoId>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, rel, fp)| !listed.contains(rel.as_str()) && !has_master(fp))
            .map(|(id, _, _)| id)
            .collect();
        drop(stmt);
        for chunk in gone.chunks(500) {
            let list = placeholders(chunk.len());
            for table in ["decisions", "decision_log", "outcomes", "analysis", "file_index"] {
                self.conn.execute(&format!("DELETE FROM {table} WHERE photo_id IN ({list})"), params_from_iter(chunk))?;
            }
            self.conn.execute(&format!("DELETE FROM photos WHERE id IN ({list})"), params_from_iter(chunk))?;
        }
        Ok(gone.len() as u64)
    }

    /// Re-attach a previously indexed photo to this location (cheap rescan path).
    pub fn touch_photo(&self, id: PhotoId, source_id: &str, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE photos SET source_id = ?2, rel_path = ?3, last_seen = ?4 WHERE id = ?1",
            params![id, source_id, rel_path, now()],
        )?;
        Ok(())
    }

    /// Photos in the given sources taken on one of `days` (`YYYY-MM-DD`, or `"unknown"` for photos
    /// without a capture date): exactly what `query` shows for that `Filter::days`.
    pub fn photo_ids_on_days(&self, source_ids: &[String], days: &[String]) -> Result<Vec<PhotoId>> {
        if source_ids.is_empty() {
            return Ok(vec![]);
        }
        let (condition, day_args) = days_condition(days);
        let mut args: Vec<String> = source_ids.to_vec();
        args.extend(day_args);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT p.id FROM photos p WHERE p.source_id IN ({}) AND {condition} ORDER BY p.id",
            placeholders(source_ids.len())
        ))?;
        let ids = stmt.query_map(params_from_iter(&args), |r| r.get::<_, PhotoId>(0))?;
        Ok(ids.collect::<rusqlite::Result<_>>()?)
    }

    /// Record that these photos were exported to `destination`.
    pub fn mark_exported(&mut self, ids: &[PhotoId], destination: &str) -> Result<()> {
        let at = now();
        let tx = self.conn.transaction()?;
        for id in ids {
            tx.execute("INSERT INTO exports (photo_id, destination, exported_at) VALUES (?1, ?2, ?3)", params![id, destination, at])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Which of `ids` have been exported at least once.
    pub fn exported_ids(&self, ids: &[PhotoId]) -> Result<std::collections::HashSet<PhotoId>> {
        let mut out = std::collections::HashSet::new();
        for chunk in ids.chunks(500) {
            let mut stmt = self.conn.prepare(&format!("SELECT DISTINCT photo_id FROM exports WHERE photo_id IN ({})", placeholders(chunk.len())))?;
            for r in stmt.query_map(params_from_iter(chunk), |r| r.get::<_, PhotoId>(0))? {
                out.insert(r?);
            }
        }
        Ok(out)
    }

    /// All saved tags, by name.
    pub fn list_tags(&self) -> Result<Vec<SavedTag>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM day_tags ORDER BY name COLLATE NOCASE")?;
        let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut days = self.conn.prepare("SELECT day FROM day_tag_days WHERE tag_id = ?1 ORDER BY day")?;
        rows.into_iter()
            .map(|(id, name)| Ok(SavedTag { id, name, days: days.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()? }))
            .collect()
    }

    /// Create the tag `name` (case-insensitive) or replace its days. Returns it.
    pub fn save_tag(&mut self, name: &str, days: &[String]) -> Result<SavedTag> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Other("a tag needs a name".into()));
        }
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO day_tags (name, created_at) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING", params![name, now()])?;
        let id: i64 = tx.query_row("SELECT id FROM day_tags WHERE name = ?1", [name], |r| r.get(0))?;
        tx.execute("DELETE FROM day_tag_days WHERE tag_id = ?1", [id])?;
        for d in days {
            tx.execute("INSERT OR IGNORE INTO day_tag_days (tag_id, day) VALUES (?1, ?2)", params![id, d])?;
        }
        tx.commit()?;
        self.list_tags()?.into_iter().find(|t| t.id == id).ok_or_else(|| CoreError::Other("tag vanished".into()))
    }

    /// Forget a tag. Photos and decisions are untouched.
    pub fn delete_tag(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM day_tag_days WHERE tag_id = ?1", [id])?;
        self.conn.execute("DELETE FROM day_tags WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Remove decisions, decision history and logged outcomes of `ids`. Returns the decisions removed.
    pub fn forget_decisions(&self, ids: &[PhotoId]) -> Result<u64> {
        let mut removed = 0;
        for chunk in ids.chunks(500) {
            let list = placeholders(chunk.len());
            removed += self.conn.execute(&format!("DELETE FROM decisions WHERE photo_id IN ({list})"), params_from_iter(chunk))? as u64;
            self.conn.execute(&format!("DELETE FROM decision_log WHERE photo_id IN ({list})"), params_from_iter(chunk))?;
            self.conn.execute(&format!("DELETE FROM outcomes WHERE photo_id IN ({list})"), params_from_iter(chunk))?;
        }
        Ok(removed)
    }

    /// Remove every analysis result of `ids`. Returns the rows removed.
    pub fn forget_analysis(&self, ids: &[PhotoId]) -> Result<u64> {
        let mut removed = 0;
        for chunk in ids.chunks(500) {
            removed += self.conn.execute(&format!("DELETE FROM analysis WHERE photo_id IN ({})", placeholders(chunk.len())), params_from_iter(chunk))? as u64;
        }
        Ok(removed)
    }

    /// Append outcomes (one row each). Rows are never updated or deleted.
    pub fn insert_outcomes(&self, entries: &[OutcomeEntry]) -> Result<()> {
        let at = now();
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO outcomes (photo_id, plan_version, question, place, suggestion, decision, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for e in entries {
            let suggestion = serde_json::to_string(&e.item).map_err(|err| CoreError::Other(err.to_string()))?;
            stmt.execute(params![e.item.photo_id, e.plan_version, e.item.question.as_str(), e.place.as_str(), suggestion, e.decision.as_str(), at])?;
        }
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
        if let Some(days) = &filter.days {
            let (condition, day_args) = days_condition(days);
            sql.push_str(&format!(" AND {condition}"));
            args.extend(day_args);
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

    // ---- analysis -----------------------------------------------------

    /// Store every result (or error) in `a`, replacing earlier rows for those kinds.
    /// Store step results (one row per kind, replacing an older row of the same kind).
    pub fn save_results(&self, photo_id: PhotoId, results: &[StepResult]) -> Result<()> {
        let at = now();
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO analysis (photo_id, kind, implementation, result, error, computed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(photo_id, kind) DO UPDATE SET implementation = excluded.implementation, result = excluded.result,
                 error = excluded.error, computed_at = excluded.computed_at",
        )?;
        for r in results {
            let (result, error) = match &r.result {
                Ok(v) if v.is_null() => (None, None),
                Ok(v) => (Some(v.to_string()), None),
                Err(e) => (None, Some(e.clone())),
            };
            stmt.execute(params![photo_id, r.kind, r.implementation, result, error, at])?;
        }
        Ok(())
    }

    /// Every stored (kind → implementation) per photo, including kinds this build doesn't know.
    pub fn result_index(&self, ids: &[PhotoId]) -> Result<HashMap<PhotoId, HashMap<String, String>>> {
        let mut out: HashMap<PhotoId, HashMap<String, String>> = HashMap::new();
        for chunk in ids.chunks(500) {
            let mut stmt = self.conn.prepare(&format!("SELECT photo_id, kind, implementation FROM analysis WHERE photo_id IN ({})", placeholders(chunk.len())))?;
            let rows = stmt.query_map(params_from_iter(chunk), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            for row in rows {
                let (id, kind, implementation) = row?;
                out.entry(id).or_default().insert(kind, implementation);
            }
        }
        Ok(out)
    }

    pub fn load_analyses(&self, ids: &[PhotoId], kinds: Option<&[AnalysisKind]>) -> Result<HashMap<PhotoId, PhotoAnalysis>> {
        let mut out: HashMap<PhotoId, PhotoAnalysis> = ids.iter().map(|id| (*id, PhotoAnalysis::new(*id))).collect();
        for chunk in ids.chunks(500) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT photo_id, kind, implementation, result, error FROM analysis WHERE photo_id IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(params_from_iter(chunk), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?))
            })?;
            for row in rows {
                let (id, kind, implementation, result, error) = row?;
                let Some(kind) = AnalysisKind::parse(&kind) else { continue };
                if kinds.is_some_and(|k| !k.contains(&kind)) {
                    continue;
                }
                let a = out.entry(id).or_insert_with(|| PhotoAnalysis::new(id));
                a.implementations.insert(kind, implementation);
                match (result, error) {
                    (Some(json), _) => match serde_json::from_str(&json).map_err(|e| e.to_string()).and_then(|v| a.set_result_json(kind, v).map_err(|e| e.to_string())) {
                        Ok(()) => {}
                        // A result written by an older format: report it like an error so it is recomputed on `force`.
                        Err(e) => {
                            a.errors.insert(kind, format!("unreadable stored result: {e}"));
                        }
                    },
                    (None, Some(e)) => {
                        a.errors.insert(kind, e);
                    }
                    (None, None) => {}
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database written by earlier builds (sources with the removed scan_* columns, photos without
    /// the sharpness column) opens as-is: every photo, decision and history row is kept and usable.
    #[test]
    fn databases_from_earlier_builds_open_with_everything_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE sources (id TEXT PRIMARY KEY, display_name TEXT NOT NULL, added_at TEXT NOT NULL, last_scan TEXT,
                     scan_files INTEGER, scan_bytes INTEGER, scan_photos INTEGER, scan_raw INTEGER, scan_other INTEGER, scan_indexed INTEGER);
                 CREATE TABLE photos (id INTEGER PRIMARY KEY, fingerprint TEXT NOT NULL UNIQUE, source_id TEXT NOT NULL, rel_path TEXT NOT NULL,
                     file_name TEXT NOT NULL, kind TEXT NOT NULL, size INTEGER NOT NULL, captured_at TEXT, camera TEXT, last_seen TEXT NOT NULL);
                 CREATE TABLE decisions (photo_id INTEGER PRIMARY KEY REFERENCES photos(id), decision TEXT NOT NULL, decided_at TEXT NOT NULL);
                 CREATE TABLE decision_log (id INTEGER PRIMARY KEY, photo_id INTEGER NOT NULL REFERENCES photos(id), decision TEXT, previous TEXT, at TEXT NOT NULL);
                 INSERT INTO sources VALUES ('/card', 'card', '2026-01-01T00:00:00', NULL, 3, 300, 3, 0, 0, 3);
                 INSERT INTO photos VALUES (1, 'aaaa', '/card', 'DCIM/A.JPG', 'A.JPG', 'jpeg', 100, '2026-01-01T10:00:00', NULL, '2026-01-01T00:00:00');
                 INSERT INTO decisions VALUES (1, 'favorite', '2026-01-02T00:00:00');
                 INSERT INTO decision_log VALUES (1, 1, 'favorite', NULL, '2026-01-02T00:00:00');",
            )
            .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.photo(1).unwrap().decision, Some(Decision::Favorite));
        assert_eq!(store.query(&["/card".into()], &Filter { status: StatusFilter::Keepers, ..Default::default() }).unwrap().len(), 1);
        // Still fully usable: new sources, photos and decisions (and undo) work.
        store.upsert_source("/other", "other").unwrap();
        let b = photo(&store, "bbbb", "/other", "DCIM/B.JPG", None);
        assert_eq!(store.set_decision(b, Some(Decision::Like)).unwrap(), None);
        assert_eq!(store.set_decision(1, None).unwrap(), Some(Decision::Favorite));
        let log: i64 = store.conn.query_row("SELECT COUNT(*) FROM decision_log", [], |r| r.get(0)).unwrap();
        assert_eq!(log, 3, "history kept and appended");
        // Tables added later (analysis, outcomes) are created on open.
        store.insert_outcomes(&[]).unwrap();
        assert_eq!(store.forget_analysis(&[1]).unwrap(), 0);
    }

    #[test]
    fn reset_picks_exactly_the_photos_the_days_filter_shows() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_source("/card", "card").unwrap();
        photo(&store, "1", "/card", "1.JPG", Some("2026-07-01T10:00:00"));
        photo(&store, "2", "/card", "2.JPG", Some("2026-07-02T23:59:59"));
        photo(&store, "3", "/card", "3.JPG", Some("2026-07-04T00:00:00"));
        photo(&store, "4", "/card", "4.JPG", None);
        // Another card, same days, not in this session: never shown, never reset.
        store.upsert_source("/other", "other").unwrap();
        photo(&store, "5", "/other", "5.JPG", Some("2026-07-01T11:00:00"));
        photo(&store, "6", "/other", "6.JPG", None);
        let src = vec!["/card".to_string()];
        for days in [vec![], vec!["2026-07-01"], vec!["2026-07-02", "2026-07-04"], vec!["unknown"], vec!["2026-07-03"], vec!["2026-07-01", "unknown"]] {
            let days: Vec<String> = days.into_iter().map(String::from).collect();
            let mut shown: Vec<PhotoId> = store.query(&src, &Filter { days: Some(days.clone()), status: StatusFilter::All }).unwrap().iter().map(|p| p.id).collect();
            shown.sort();
            assert_eq!(store.photo_ids_on_days(&src, &days).unwrap(), shown, "{days:?}");
            assert!(store.photo_ids_on_days(&[], &days).unwrap().is_empty(), "no sources in the session: nothing");
        }
    }

    #[test]
    fn forgetting_a_day_removes_its_decisions_history_outcomes_and_analysis_only() {
        use crate::analysis::pipeline::StepResult;
        use crate::triage::{Place, Question, TriageItem};
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_source("/card", "card").unwrap();
        let a = photo(&store, "aaaa", "/card", "A.JPG", Some("2026-07-01T10:00:00"));
        let b = photo(&store, "bbbb", "/card", "B.JPG", Some("2026-07-02T10:00:00"));
        let nodate = photo(&store, "cccc", "/card", "C.JPG", None);
        let src = vec!["/card".to_string()];
        for id in [a, b, nodate] {
            store.set_decision(id, Some(Decision::Like)).unwrap();
            store.save_results(id, &[StepResult { kind: "blur".into(), implementation: "x".into(), result: Ok(serde_json::json!({})) }]).unwrap();
            let item = TriageItem { photo_id: id, question: Question::Judge, group: None, reasons: vec![], hints: vec![], score: None, analyzed: true };
            store.insert_outcomes(&[OutcomeEntry { item, plan_version: "v".into(), place: Place::Timeline, decision: Decision::Like }]).unwrap();
        }
        let count = |table: &str, id: PhotoId| -> i64 { store.conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE photo_id = ?1"), [id], |r| r.get(0)).unwrap() };

        let ids = store.photo_ids_on_days(&src, &["2026-07-01".into()]).unwrap();
        assert_eq!(ids, vec![a]);
        assert_eq!(store.forget_decisions(&ids).unwrap(), 1);
        for table in ["decisions", "decision_log", "outcomes"] {
            assert_eq!(count(table, a), 0, "{table} of the reset day");
            assert_eq!((count(table, b), count(table, nodate)), (1, 1), "{table} of other photos untouched");
        }
        assert_eq!(count("analysis", a), 1, "analysis is a separate choice");
        assert_eq!(store.forget_analysis(&ids).unwrap(), 1);
        assert_eq!((count("analysis", a), count("analysis", b)), (0, 1));
        assert_eq!(store.photo(a).unwrap().decision, None, "the photo itself is kept");
        assert!(store.photo_ids_on_days(&src, &[]).unwrap().is_empty());
        assert_eq!(store.photo_ids_on_days(&src, &["unknown".into()]).unwrap(), vec![nodate], "'unknown' = no capture date, as in Filter");
        let mut both = store.photo_ids_on_days(&src, &["2026-07-02".into(), "unknown".into()]).unwrap();
        both.sort();
        assert_eq!(both, vec![b, nodate]);
    }

    #[test]
    fn outcomes_are_appended_with_the_suggestion_as_json() {
        use crate::triage::{Place, Question, TriageItem};
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_source("/card", "card").unwrap();
        let id = photo(&store, "aaaa", "/card", "A.JPG", None);
        let item = |question| TriageItem { photo_id: id, question, group: None, reasons: vec![crate::analysis::IssueKind::Blurry], hints: vec![], score: Some(0.2), analyzed: true };
        let entry = |question, place, decision| OutcomeEntry { item: item(question), plan_version: "triage.v1".into(), place, decision };
        store
            .transaction(|s| s.insert_outcomes(&[entry(Question::Sweep, Place::Rescue, Decision::Like), entry(Question::Judge, Place::Timeline, Decision::Dislike)]))
            .unwrap();
        store.insert_outcomes(&[entry(Question::Sweep, Place::OpenedEarly, Decision::Favorite)]).unwrap();

        let mut stmt = store.conn.prepare("SELECT question, place, decision, suggestion FROM outcomes ORDER BY id").unwrap();
        let rows: Vec<(String, String, String, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        let short: Vec<_> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str(), r.2.as_str())).collect();
        assert_eq!(short, [("sweep", "rescue", "like"), ("judge", "timeline", "dislike"), ("sweep", "openedEarly", "favorite")]);
        let back: TriageItem = serde_json::from_str(&rows[0].3).unwrap();
        assert_eq!(back, item(Question::Sweep), "the suggestion round-trips");
    }

    /// Two copies of the app on one database: the second waits for the first's write instead of failing.
    #[test]
    fn a_busy_database_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut first = Store::open(&path).unwrap();
        let second = Store::open(&path).unwrap();
        let writer = std::thread::spawn(move || {
            first
                .transaction(|s| {
                    s.upsert_source("/a", "a")?;
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    Ok(())
                })
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        second.upsert_source("/b", "b").unwrap();
        writer.join().unwrap();
        assert_eq!(second.all_source_ids().unwrap().len(), 2);
    }

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
        let all = s.query(&src, &Filter { days: None, status: StatusFilter::All }).unwrap();
        assert_eq!(all.iter().map(|p| p.file_name.as_str()).collect::<Vec<_>>(), ["2.jpg", "1.jpg", "3.jpg", "4.jpg"]);

        s.set_decision(a, Some(Decision::Like)).unwrap();
        s.set_decision(b, Some(Decision::Hate)).unwrap();
        s.set_decision(c, Some(Decision::Favorite)).unwrap();

        let undecided = s.query(&src, &Filter::default()).unwrap();
        assert_eq!(undecided.len(), 1);
        let keepers = s.query(&src, &Filter { days: None, status: StatusFilter::Keepers }).unwrap();
        assert_eq!(keepers.len(), 2);
        let on = |days: &[&str]| Filter { days: Some(days.iter().map(|d| d.to_string()).collect()), status: StatusFilter::All };
        let day1 = s.query(&src, &on(&["2026-07-01"])).unwrap();
        assert_eq!(day1.len(), 2);

        let st = s.stats(&src).unwrap();
        assert_eq!(st, Stats { total: 4, undecided: 1, like: 1, dislike: 0, favorite: 1, hate: 1 });

        let days = s.days(&src).unwrap();
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].day.as_deref(), Some("2026-07-01"));
        assert_eq!(days[2].day, None);

        // Any set of days, not just a range; "unknown" = no capture date; empty = nothing.
        assert_eq!(s.query(&src, &on(&["2026-07-01", "2026-07-02"])).unwrap().len(), 3);
        let skip = s.query(&src, &on(&["2026-07-02", "2026-07-09"])).unwrap();
        assert_eq!(skip.iter().map(|p| p.file_name.as_str()).collect::<Vec<_>>(), ["3.jpg"]);
        assert_eq!(s.query(&src, &on(&["unknown"])).unwrap().len(), 1);
        assert_eq!(s.query(&src, &on(&["2026-07-02", "unknown"])).unwrap().len(), 2);
        assert!(s.query(&src, &on(&[])).unwrap().is_empty());

        // Undo = restore previous.
        let prev = s.set_decision(c, None).unwrap();
        assert_eq!(prev, Some(Decision::Favorite));
        assert_eq!(s.decision(c).unwrap(), None);
    }

    #[test]
    fn bulk_decisions() {
        let mut s = Store::open_in_memory().unwrap();
        let a = photo(&s, "1", "a", "x/1.jpg", Some("2026-07-01T10:00:00"));
        let b = photo(&s, "2", "a", "x/2.jpg", Some("2026-07-01T10:00:01"));
        s.set_decision(a, Some(Decision::Like)).unwrap();
        let prev = s.set_many(&[(a, Some(Decision::Dislike)), (b, Some(Decision::Dislike))]).unwrap();
        assert_eq!(prev, vec![Some(Decision::Like), None]);
        assert_eq!(s.decision(b).unwrap(), Some(Decision::Dislike));
    }
}
