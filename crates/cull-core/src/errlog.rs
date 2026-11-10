//! Error log for background work (analysis), so problems are visible while developing and testing.
//!
//! A plain text file, one line per error: `2026-11-10T14:02:11 [analysis] photo 42 (DSC_0042.JPG): …`.
//! At [`MAX_BYTES`] it rotates: `errors.log` becomes `errors.1.log` (replacing an older one) and a new
//! file starts, so the log never takes more than about twice that. The platform chooses the folder
//! (the app's own log folder); nothing is ever written next to photos.
//! Writing is best effort: a log that can't be written is reported on stderr and never fails the caller.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Rotate when the file reaches this size.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// `recent` reads at most this much from the end of the file.
const TAIL_BYTES: u64 = 256 * 1024;

pub struct ErrorLog {
    path: PathBuf,
    max_bytes: u64,
    /// Keys already logged in this app session (`record_once`).
    seen: Mutex<HashSet<String>>,
    /// One writer at a time, so rotation and appends don't interleave.
    writing: Mutex<()>,
}

impl ErrorLog {
    pub fn new(path: PathBuf) -> Self {
        Self::with_max_bytes(path, MAX_BYTES)
    }

    fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self {
        Self { path, max_bytes, seen: Mutex::new(HashSet::new()), writing: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The previous file, kept after a rotation.
    fn rotated_path(&self) -> PathBuf {
        self.path.with_extension("1.log")
    }

    /// Append one line (also printed to stderr).
    pub fn record(&self, area: &str, message: &str) {
        let line = format!("{} [{area}] {}", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"), message.replace('\n', " "));
        eprintln!("{line}");
        if let Err(e) = self.append(&line) {
            eprintln!("could not write the error log {}: {e}", self.path.display());
        }
    }

    /// Like `record`, but only the first time `key` is seen in this app session, so a photo that fails
    /// on every pass is logged once, not on every retry.
    pub fn record_once(&self, key: &str, area: &str, message: &str) {
        if self.seen.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_string()) {
            self.record(area, message);
        }
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        let _one_writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::metadata(&self.path).is_ok_and(|m| m.len() >= self.max_bytes) {
            let rotated = self.rotated_path();
            fs::rename(&self.path, &rotated)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(file, "{line}")
    }

    /// The last `max_lines` lines, oldest first (reads only the end of the file).
    pub fn recent(&self, max_lines: usize) -> Vec<String> {
        let Ok(mut file) = fs::File::open(&self.path) else { return Vec::new() };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let start = len.saturating_sub(TAIL_BYTES);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_to_end(&mut bytes)).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        // When reading from the middle, the first line is probably cut: drop it.
        let lines: Vec<&str> = text.lines().skip(usize::from(start > 0)).collect();
        lines[lines.len().saturating_sub(max_lines)..].iter().map(|l| l.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines_and_reads_back_the_most_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let log = ErrorLog::new(tmp.path().join("logs").join("errors.log"));
        assert!(log.recent(10).is_empty(), "no file yet");
        for n in 0..5 {
            log.record("analysis", &format!("problem {n}\nsecond line"));
        }
        let recent = log.recent(2);
        assert_eq!(recent.len(), 2);
        assert!(recent[0].ends_with("[analysis] problem 3 second line"), "{recent:?}");
        assert!(recent[1].contains("problem 4"));
    }

    #[test]
    fn record_once_logs_a_key_only_once_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        let log = ErrorLog::new(tmp.path().join("errors.log"));
        for _ in 0..3 {
            log.record_once("photo:1", "analysis", "photo 1 has no preview");
        }
        log.record_once("photo:2", "analysis", "photo 2 has no preview");
        assert_eq!(log.recent(10).len(), 2);
    }

    #[test]
    fn rotates_at_the_size_limit_keeping_one_previous_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("errors.log");
        let log = ErrorLog::with_max_bytes(path.clone(), 200);
        for n in 0..20 {
            log.record("analysis", &format!("a fairly long error message number {n}"));
        }
        let rotated = tmp.path().join("errors.1.log");
        assert!(rotated.exists(), "the full file was kept as errors.1.log");
        assert!(fs::metadata(&path).unwrap().len() < 200 + 100, "the current file starts over");
        assert!(fs::metadata(&rotated).unwrap().len() < 200 + 100, "only one generation is kept, each about the limit");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2, "never more than two files");
        assert!(log.recent(1)[0].contains("number 19"));
    }
}
