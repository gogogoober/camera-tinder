//! Index a `MediaSource`: the one moment the app reads the originals for culling.
//!
//! Each new file is read from the source **exactly once**, into memory. From those
//! bytes we compute the fingerprint, EXIF, sharpness and the cached previews, so
//! after a scan the whole culling session runs from the cache and the card sits idle.
//! (Export is the only other moment the originals are read.)
//!
//! Three passes so the database lock is never held during slow I/O:
//! 1. cheap lookups of already-indexed files (path + size + mtime; no file reads),
//! 2. one read per new/changed file (or older rows missing previews/sharpness),
//! 3. one transaction to insert the results.

use std::io::{Cursor, Read};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, NaiveDateTime};
use rayon::prelude::*;

use crate::error::{CoreError, Result};
use crate::identity::fingerprint;
use crate::metadata::{PhotoMeta, read_meta};
use crate::preview::{self, PreviewCache};
use crate::sharpness;
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

/// Files read from the source at the same time. Bounds memory (a 45 MP JPEG is ~30 MB
/// on disk and ~135 MB decoded) and keeps card I/O mostly sequential.
const INGEST_THREADS: usize = 4;

/// Read one file from the source, once, and derive everything from those bytes.
fn ingest(source: &dyn MediaSource, rel_path: &str, size: u64, previews: &PreviewCache) -> Result<(String, PhotoMeta, Option<u8>)> {
    let mut bytes = Vec::with_capacity(size as usize);
    source.open(rel_path)?.read_to_end(&mut bytes)?; // the only source read
    if bytes.len() as u64 != size {
        return Err(CoreError::Other(format!("file changed while reading ({} bytes, listed as {size})", bytes.len())));
    }
    let mut cur = Cursor::new(&bytes[..]);
    let fp = fingerprint(&mut cur, size)?;
    let meta = read_meta(&mut cur);
    // A file that can't be decoded is still indexed (it's still a photo on the card);
    // it just has no sharpness or preview.
    let sharp = match preview::decode(&bytes) {
        Ok(img) => {
            previews.store_from_image(&fp, &img)?;
            Some(sharpness::score_image(&img))
        }
        Err(_) => None,
    };
    Ok((fp, meta, sharp))
}

pub fn scan_source(store: &Mutex<Store>, source: &dyn MediaSource, previews: &PreviewCache, sink: &dyn EventSink) -> Result<ScanReport> {
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
            None => {
                report.other_files += 1;
                None
            }
        })
        .collect();
    let total = candidates.len() as u64;
    report.found = total;

    // Pass 1: skip files we've already indexed and that haven't changed.
    let mut misses = Vec::new();
    // Already-indexed photos missing sharpness or cached previews (older versions) are re-read
    // once now, so they never need the source during culling.
    let mut backfill: Vec<(i64, String, u64, String)> = Vec::new();
    store.lock().unwrap().transaction(|s| {
        for (e, k) in candidates {
            let mt = mtime_secs(e.modified);
            match s.lookup_file(&src, &e.rel_path, e.size, mt)? {
                Some(id) => {
                    s.touch_photo(id, &src, &e.rel_path)?;
                    let fp = s.photo(id)?.fingerprint;
                    if s.needs_sharpness(id)? || !previews.has_master(&fp) {
                        backfill.push((id, e.rel_path.clone(), e.size, fp));
                    }
                }
                None => misses.push((e, k, mt)),
            }
        }
        Ok(())
    })?;

    let done = AtomicU64::new(total - misses.len() as u64);
    sink.emit(CoreEvent::ScanProgress { source_id: src.clone(), done: done.load(Ordering::Relaxed), total });

    // Pass 2: read new files (a few at a time), without holding the DB lock.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(INGEST_THREADS).build().map_err(|e| CoreError::Other(e.to_string()))?;
    let (analyzed, backfilled): (Vec<_>, Vec<(i64, u8)>) = pool.install(|| {
        let analyzed = misses
            .into_par_iter()
            .map(|(e, k, mt)| {
                let result = ingest(source, &e.rel_path, e.size, previews);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 25 == 0 || n == total {
                    sink.emit(CoreEvent::ScanProgress { source_id: src.clone(), done: n, total });
                }
                (e, k, mt, result)
            })
            .collect();
        let backfilled = backfill
            .into_par_iter()
            .filter_map(|(id, rel, size, fp)| match ingest(source, &rel, size, previews) {
                // Same photo as indexed: keep its score. A changed file is re-indexed on the next scan.
                Ok((new_fp, _, Some(score))) if new_fp == fp => Some((id, score)),
                _ => None,
            })
            .collect();
        (analyzed, backfilled)
    });

    // Pass 3: persist.
    let mut st = store.lock().unwrap();
    st.transaction(|s| {
        for (e, kind, mt, result) in analyzed {
            match result {
                Ok((fp, meta, sharp)) => {
                    let captured_at = meta.captured_at.or_else(|| e.modified.map(local_naive));
                    let (id, is_new) = s.upsert_photo(&NewPhoto {
                        fingerprint: &fp,
                        source_id: &src,
                        rel_path: &e.rel_path,
                        kind,
                        size: e.size,
                        captured_at,
                        camera: meta.camera.as_deref(),
                        sharpness: sharp,
                    })?;
                    s.record_file(&src, &e.rel_path, e.size, mt, id)?;
                    if is_new {
                        report.new += 1;
                    }
                }
                Err(err) => report.failed.push((e.rel_path, err.to_string())),
            }
        }
        for (id, score) in backfilled {
            s.set_photo_sharpness(id, score)?;
        }
        Ok(())
    })?;
    st.mark_scanned(&src)?;
    Ok(report)
}
