//! Offload tables: the backup drive's fingerprint cache, remembered folders, and every run's files.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::error::Result;
use crate::store::Store;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS offload_library_files (
    library TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (library, rel_path)
);
CREATE TABLE IF NOT EXISTS offload_folders (
    library_id TEXT NOT NULL,
    brand TEXT NOT NULL,
    year INTEGER NOT NULL,
    key TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    PRIMARY KEY (library_id, brand, year, key)
);
CREATE TABLE IF NOT EXISTS offload_runs (
    id INTEGER PRIMARY KEY,
    library_id TEXT NOT NULL,
    library_root TEXT NOT NULL,
    card_root TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    copied INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS offload_items (
    run_id INTEGER NOT NULL REFERENCES offload_runs(id),
    src_rel TEXT NOT NULL,
    size INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    hash TEXT,
    partial_rel TEXT,
    dest_rel TEXT NOT NULL,
    brand TEXT NOT NULL,
    year INTEGER NOT NULL,
    folder_key TEXT NOT NULL,
    day TEXT,
    state TEXT NOT NULL,
    error TEXT,
    PRIMARY KEY (run_id, src_rel)
);
CREATE INDEX IF NOT EXISTS offload_items_state ON offload_items(state);
"#;

/// A cached fingerprint of a file on the backup drive, valid while its size and mtime are unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedFile {
    pub size: u64,
    pub mtime: Option<i64>,
    pub fingerprint: String,
}

/// One file of a run, as recorded when it starts.
pub struct ItemStart<'a> {
    pub src_rel: &'a str,
    pub size: u64,
    pub fingerprint: &'a str,
    pub partial_rel: &'a str,
    pub dest_rel: &'a str,
    pub brand: &'a str,
    pub year: i32,
    pub folder_key: &'a str,
    pub day: Option<String>,
}

/// Newest backup per camera brand on one drive.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandBackup {
    pub brand: String,
    /// Latest photo day backed up (`YYYY-MM-DD`), if any had a date.
    pub last_day: Option<String>,
    /// When that backup ran (local time, `YYYY-MM-DDTHH:MM:SS`).
    pub at: String,
}

fn now() -> String {
    chrono::Local::now().naive_local().format("%Y-%m-%dT%H:%M:%S").to_string()
}

impl Store {
    pub fn offload_cached_files(&self, library: &str) -> Result<HashMap<String, CachedFile>> {
        let mut stmt = self.conn.prepare("SELECT rel_path, size, mtime, fingerprint FROM offload_library_files WHERE library = ?1")?;
        let rows = stmt.query_map([library], |r| {
            Ok((r.get::<_, String>(0)?, CachedFile { size: r.get::<_, i64>(1)? as u64, mtime: r.get(2)?, fingerprint: r.get(3)? }))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn offload_cache_files(&mut self, library: &str, files: &[(String, CachedFile)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO offload_library_files (library, rel_path, size, mtime, fingerprint) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(library, rel_path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, fingerprint = excluded.fingerprint",
            )?;
            for (rel, f) in files {
                stmt.execute(params![library, rel, f.size as i64, f.mtime, f.fingerprint])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remembered folders of one drive: (brand, year, key) → folder path.
    pub fn offload_folders(&self, library_id: &str) -> Result<HashMap<(String, i32, String), String>> {
        let mut stmt = self.conn.prepare("SELECT brand, year, key, rel_path FROM offload_folders WHERE library_id = ?1")?;
        let rows = stmt.query_map([library_id], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn offload_remember_folder(&self, library_id: &str, brand: &str, year: i32, key: &str, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO offload_folders (library_id, brand, year, key, rel_path) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(library_id, brand, year, key) DO UPDATE SET rel_path = excluded.rel_path",
            params![library_id, brand, year, key, rel_path],
        )?;
        Ok(())
    }

    /// Files copied into a remembered folder, (path, size), to find the folder again after a rename in Finder.
    pub fn offload_files_in_folder(&self, library_id: &str, brand: &str, year: i32, key: &str) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.dest_rel, i.size FROM offload_items i JOIN offload_runs r ON r.id = i.run_id
             WHERE r.library_id = ?1 AND i.brand = ?2 AND i.year = ?3 AND i.folder_key = ?4 AND i.state = 'verified' LIMIT 50",
        )?;
        let rows = stmt.query_map(params![library_id, brand, year, key], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn offload_start_run(&self, library_id: &str, library_root: &str, card_root: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO offload_runs (library_id, library_root, card_root, started_at) VALUES (?1, ?2, ?3, ?4)",
            params![library_id, library_root, card_root, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn offload_finish_run(&self, run_id: i64, status: &str, copied: u64, bytes: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE offload_runs SET finished_at = ?2, status = ?3, copied = ?4, bytes = ?5 WHERE id = ?1",
            params![run_id, now(), status, copied as i64, bytes as i64],
        )?;
        Ok(())
    }

    /// Recorded before the first byte is written, so a crash leaves a trace of the temp file.
    pub fn offload_item_started(&self, run_id: i64, it: &ItemStart<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO offload_items (run_id, src_rel, size, fingerprint, partial_rel, dest_rel, brand, year, folder_key, day, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'writing')",
            params![run_id, it.src_rel, it.size as i64, it.fingerprint, it.partial_rel, it.dest_rel, it.brand, it.year, it.folder_key, it.day],
        )?;
        Ok(())
    }

    pub fn offload_item_verified(&self, run_id: i64, src_rel: &str, hash: u128) -> Result<()> {
        self.conn.execute(
            "UPDATE offload_items SET state = 'verified', hash = ?3, partial_rel = NULL WHERE run_id = ?1 AND src_rel = ?2",
            params![run_id, src_rel, format!("{hash:032x}")],
        )?;
        Ok(())
    }

    /// `partial_gone`: the temp file was removed (otherwise it stays listed for cleanup).
    pub fn offload_item_failed(&self, run_id: i64, src_rel: &str, error: &str, partial_gone: bool) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE offload_items SET state = 'failed', error = ?3{} WHERE run_id = ?1 AND src_rel = ?2",
                if partial_gone { ", partial_rel = NULL" } else { "" }
            ),
            params![run_id, src_rel, error],
        )?;
        Ok(())
    }

    /// Temp files an earlier run on this drive may have left (crash, unplugged drive): (run, src, temp path).
    pub fn offload_leftover_partials(&self, library_id: &str) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.run_id, i.src_rel, i.partial_rel FROM offload_items i JOIN offload_runs r ON r.id = i.run_id
             WHERE r.library_id = ?1 AND i.partial_rel IS NOT NULL",
        )?;
        let rows = stmt.query_map([library_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn offload_partial_cleaned(&self, run_id: i64, src_rel: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE offload_items SET partial_rel = NULL, state = CASE WHEN state = 'writing' THEN 'abandoned' ELSE state END
             WHERE run_id = ?1 AND src_rel = ?2",
            params![run_id, src_rel],
        )?;
        Ok(())
    }

    /// Runs that never finished (crash): marked so they don't look like they are still going.
    pub fn offload_close_stale_runs(&self) -> Result<()> {
        self.conn.execute("UPDATE offload_runs SET status = 'interrupted' WHERE status = 'running'", [])?;
        Ok(())
    }

    pub fn offload_last_backups(&self, library_id: &str) -> Result<Vec<BrandBackup>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.brand, MAX(i.day), MAX(COALESCE(r.finished_at, r.started_at)) FROM offload_items i JOIN offload_runs r ON r.id = i.run_id
             WHERE r.library_id = ?1 AND i.state = 'verified' GROUP BY i.brand ORDER BY i.brand",
        )?;
        let rows = stmt.query_map([library_id], |r| Ok(BrandBackup { brand: r.get(0)?, last_day: r.get(1)?, at: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn offload_run_status(&self, run_id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT status FROM offload_runs WHERE id = ?1", [run_id], |r| r.get(0)).optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        let f = CachedFile { size: 10, mtime: Some(5), fingerprint: "ab".into() };
        s.offload_cache_files("lib", &[("Photos/a.jpg".into(), f.clone())]).unwrap();
        assert_eq!(s.offload_cached_files("lib").unwrap()["Photos/a.jpg"], f);

        let run = s.offload_start_run("L1", "/Volumes/T7", "/Volumes/SD").unwrap();
        let start = |src: &'static str| ItemStart {
            src_rel: src,
            size: 10,
            fingerprint: "fp",
            partial_rel: "Photos/.x.partial",
            dest_rel: "Photos/Fuji/2026/Japan trip/x.jpg",
            brand: "Fuji",
            year: 2026,
            folder_key: "trip:japan trip",
            day: Some("2026-10-03".into()),
        };
        s.offload_item_started(run, &start("a")).unwrap();
        s.offload_item_started(run, &start("b")).unwrap();
        s.offload_item_verified(run, "a", 7).unwrap();
        assert_eq!(s.offload_leftover_partials("L1").unwrap(), vec![(run, "b".to_string(), "Photos/.x.partial".to_string())]);
        s.offload_partial_cleaned(run, "b").unwrap();
        assert!(s.offload_leftover_partials("L1").unwrap().is_empty());
        assert_eq!(s.offload_files_in_folder("L1", "Fuji", 2026, "trip:japan trip").unwrap(), vec![("Photos/Fuji/2026/Japan trip/x.jpg".to_string(), 10)]);
        s.offload_finish_run(run, "done", 1, 10).unwrap();
        let last = s.offload_last_backups("L1").unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].last_day.as_deref(), Some("2026-10-03"));
        s.offload_remember_folder("L1", "Fuji", 2026, "trip:japan trip", "Photos/Fuji/2026/Japan trip").unwrap();
        assert_eq!(s.offload_folders("L1").unwrap().len(), 1);
    }
}
