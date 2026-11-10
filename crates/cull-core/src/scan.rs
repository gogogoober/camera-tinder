//! Index a `MediaSource`: list files, fingerprint them, read EXIF.
//!
//! Three passes so the database lock is never held during slow I/O:
//! 1. cheap lookups of already-indexed files (path + size + mtime),
//! 2. parallel fingerprint + EXIF read of new/changed files,
//! 3. one transaction to insert the results.

use std::io::BufReader;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, NaiveDateTime};
use rayon::prelude::*;

use crate::error::Result;
use crate::identity::fingerprint;
use crate::metadata::{PhotoMeta, read_meta};
use crate::model::{MediaKind, ScanReport};
use crate::ports::{CoreEvent, EventSink, MediaEntry, MediaSource};
use crate::store::{NewPhoto, Store};

/// Kinds that can currently be culled. RAW joins once embedded-preview
/// extraction lands (desktop only; iOS stays JPEG-only).
pub const CULLABLE_KINDS: &[MediaKind] = &[MediaKind::Jpeg];

fn mtime_secs(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn local_naive(t: SystemTime) -> NaiveDateTime {
    DateTime::<Local>::from(t).naive_local()
}

fn analyze(source: &dyn MediaSource, entry: &MediaEntry) -> Result<(String, PhotoMeta)> {
    let mut r = BufReader::with_capacity(64 * 1024, source.open(&entry.rel_path)?);
    let fp = fingerprint(&mut r, entry.size)?;
    let meta = read_meta(&mut r);
    Ok((fp, meta))
}

pub fn scan_source(store: &Mutex<Store>, source: &dyn MediaSource, sink: &dyn EventSink) -> Result<ScanReport> {
    let src = source.id().to_string();
    let mut report = ScanReport::default();

    let candidates: Vec<(MediaEntry, MediaKind)> = source
        .list()?
        .into_iter()
        .filter_map(|e| match MediaKind::from_path(&e.rel_path) {
            Some(k) if CULLABLE_KINDS.contains(&k) => Some((e, k)),
            Some(_) => {
                report.skipped_unsupported += 1;
                None
            }
            None => None,
        })
        .collect();
    let total = candidates.len() as u64;
    report.found = total;

    // Pass 1: skip files we've already indexed and that haven't changed.
    let mut misses = Vec::new();
    store.lock().unwrap().transaction(|s| {
        for (e, k) in candidates {
            let mt = mtime_secs(e.modified);
            match s.lookup_file(&src, &e.rel_path, e.size, mt)? {
                Some(id) => s.touch_photo(id, &src, &e.rel_path)?,
                None => misses.push((e, k, mt)),
            }
        }
        Ok(())
    })?;

    let done = AtomicU64::new(total - misses.len() as u64);
    sink.emit(CoreEvent::ScanProgress { source_id: src.clone(), done: done.load(Ordering::Relaxed), total });

    // Pass 2: read new files in parallel, without holding the DB lock.
    let analyzed: Vec<_> = misses
        .into_par_iter()
        .map(|(e, k, mt)| {
            let result = analyze(source, &e);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 25 == 0 || n == total {
                sink.emit(CoreEvent::ScanProgress { source_id: src.clone(), done: n, total });
            }
            (e, k, mt, result)
        })
        .collect();

    // Pass 3: persist.
    let mut st = store.lock().unwrap();
    st.transaction(|s| {
        for (e, kind, mt, result) in analyzed {
            match result {
                Ok((fp, meta)) => {
                    let captured_at = meta.captured_at.or_else(|| e.modified.map(local_naive));
                    let (id, is_new) = s.upsert_photo(&NewPhoto {
                        fingerprint: &fp,
                        source_id: &src,
                        rel_path: &e.rel_path,
                        kind,
                        size: e.size,
                        captured_at,
                        camera: meta.camera.as_deref(),
                    })?;
                    s.record_file(&src, &e.rel_path, e.size, mt, id)?;
                    if is_new {
                        report.new += 1;
                    }
                }
                Err(err) => report.failed.push((e.rel_path, err.to_string())),
            }
        }
        Ok(())
    })?;
    st.mark_scanned(&src)?;
    Ok(report)
}
