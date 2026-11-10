//! Index a `MediaSource`: the one moment the app reads the originals for culling.
//!
//! Each new file is read from the source **exactly once**, into memory. From those
//! bytes we compute the fingerprint, EXIF and the cached previews, so after a scan the
//! whole culling session runs from the cache and the card sits idle.
//! (Export is the only other moment the originals are read.)
//!
//! Work is skipped at three levels, cheapest first:
//! 1. **Folder:** if the folder lists the same number of files with the same total size as
//!    after its last complete scan, nothing else happens (unless the scan is forced).
//! 2. **File:** files already indexed (same path + size + mtime) are not read.
//! 3. **Photo:** each new file is read and decoded once; every preview size comes from that.
//!
//! New photos are saved in batches as they are processed, so the UI can show them while the
//! scan runs, and a stopped scan keeps everything finished so far (the next scan resumes).

use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, NaiveDateTime};
use rayon::prelude::*;

use crate::error::{CoreError, Result};
use crate::identity::fingerprint;
use crate::metadata::{PhotoMeta, read_meta};
use crate::model::{MediaKind, ScanReport};
use crate::ports::{CoreEvent, EventSink, MediaEntry, MediaSource};
use crate::preview::PreviewCache;
use crate::store::{FolderSignature, NewPhoto, Store};

/// Kinds that can currently be culled. RAW joins once embedded-preview
/// extraction lands (desktop only; iOS stays JPEG-only).
pub const CULLABLE_KINDS: &[MediaKind] = &[MediaKind::Jpeg];

/// Files read from the source at the same time. Bounds memory and keeps card I/O mostly sequential.
const INGEST_THREADS: usize = 4;
/// New photos saved per database transaction (and per "photos saved" event to the UI).
const SAVE_BATCH: usize = 32;
/// Bigger "photos" are reported as failed without being read: a whole file is held in memory while
/// it is processed (4 at a time), so a huge misnamed file must not exhaust memory. Camera JPEGs are
/// far below this (a 100 MP JPEG is ~60 MB).
pub const MAX_PHOTO_BYTES: u64 = 512 * 1024 * 1024;
/// Progress events: one every this many photos.
const PROGRESS_EVERY: u64 = 8;

fn mtime_secs(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn local_naive(t: SystemTime) -> NaiveDateTime {
    DateTime::<Local>::from(t).naive_local()
}

/// `ingest`, with a crash anywhere inside (a decoder or EXIF bug on a damaged file) reported as a
/// failure of that one file instead of ending the scan.
fn ingest_guarded(source: &dyn MediaSource, rel_path: &str, size: u64, previews: &PreviewCache) -> Result<(String, PhotoMeta)> {
    catch_unwind(AssertUnwindSafe(|| ingest(source, rel_path, size, previews))).unwrap_or_else(|_| Err(CoreError::Other("crashed while reading this file".into())))
}

/// Read one file from the source, once, and derive everything from those bytes.
fn ingest(source: &dyn MediaSource, rel_path: &str, size: u64, previews: &PreviewCache) -> Result<(String, PhotoMeta)> {
    if size > MAX_PHOTO_BYTES {
        return Err(CoreError::Other(format!("too large to be a photo ({} MB); not read", size / (1024 * 1024))));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // The only source read. Never more than one byte past the listed size, even if the file grew.
    source.open(rel_path)?.take(size + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        return Err(CoreError::Other(format!("file changed while reading ({} bytes, listed as {size})", bytes.len())));
    }
    let mut cur = Cursor::new(&bytes[..]);
    let fp = fingerprint(&mut cur, size)?;
    let meta = read_meta(&mut cur);
    // A file that can't be decoded is still indexed (it's still a photo on the card); it just has no preview.
    previews.store_from_original(&fp, &bytes)?;
    Ok((fp, meta))
}

/// How a scan treats folders whose contents look unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderCheck {
    /// Skip the folder when its file count and total size match its last complete scan, and the
    /// photos it held then still point to it.
    SkipUnchanged,
    /// Look at every file anyway (already-indexed files are still not read).
    Force,
}

pub fn scan_source(
    store: &Mutex<Store>,
    source: &dyn MediaSource,
    previews: &PreviewCache,
    check: FolderCheck,
    cancel: &AtomicBool,
    sink: &dyn EventSink,
) -> Result<ScanReport> {
    let src = source.id().to_string();
    let mut report = ScanReport::default();

    let listed = source.list()?;
    let signature = FolderSignature { files: listed.len() as u64, bytes: listed.iter().map(|e| e.size).sum(), ..Default::default() };
    if check == FolderCheck::SkipUnchanged {
        let store = store.lock().unwrap();
        if let Some(last) = store.scan_signature(&src)? {
            if last.files == signature.files && last.bytes == signature.bytes && store.source_photo_count(&src)? == last.indexed {
                report.unchanged_sources = 1;
                report.found = last.photos;
                report.skipped_unsupported = last.raw;
                report.other_files = last.other;
                return Ok(report);
            }
        }
    }

    let candidates: Vec<(MediaEntry, MediaKind)> = listed
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

    // Skip files we've already indexed and that haven't changed (no file reads).
    let mut misses = Vec::new();
    // Already-indexed photos without cached previews (older versions, or after "Clear previews")
    // are re-read once now, so they never need the source during culling.
    let mut backfill: Vec<(String, u64, String)> = Vec::new();
    store.lock().unwrap().transaction(|s| {
        for (e, k) in candidates {
            let mt = mtime_secs(e.modified);
            match s.lookup_file(&src, &e.rel_path, e.size, mt)? {
                Some(id) => {
                    s.touch_photo(id, &src, &e.rel_path)?;
                    let fp = s.photo(id)?.fingerprint;
                    if !previews.has_master(&fp) {
                        backfill.push((e.rel_path.clone(), e.size, fp));
                    }
                }
                None => misses.push((e, k, mt)),
            }
        }
        Ok(())
    })?;

    let done = AtomicU64::new(total - misses.len() as u64);
    let progress = |n: u64| sink.emit(CoreEvent::ScanProgress { source_id: src.clone(), done: n, total });
    progress(done.load(Ordering::Relaxed));
    let tick = || {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % PROGRESS_EVERY == 0 || n == total {
            progress(n);
        }
    };

    // Work actually finished, so a Stop that arrives after the last photo doesn't count as stopping.
    let (read_new, read_backfill) = (AtomicU64::new(0), AtomicU64::new(0));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(INGEST_THREADS).build().map_err(|e| CoreError::Other(e.to_string()))?;

    // New files: read a batch (a few at a time, without the DB lock), then save it.
    for batch in misses.chunks(SAVE_BATCH) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let analyzed: Vec<_> = pool.install(|| {
            batch
                .par_iter()
                .filter_map(|(e, k, mt)| {
                    if cancel.load(Ordering::Relaxed) {
                        return None; // photos already being read finish; the rest wait for the next scan
                    }
                    let result = ingest_guarded(source, &e.rel_path, e.size, previews);
                    read_new.fetch_add(1, Ordering::Relaxed);
                    tick();
                    Some((e, *k, *mt, result))
                })
                .collect()
        });
        let saved_before = report.new;
        store.lock().unwrap().transaction(|s| {
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
                    Err(err) => report.failed.push((e.rel_path.clone(), err.to_string())),
                }
            }
            Ok(())
        })?;
        if report.new > saved_before {
            sink.emit(CoreEvent::ScanSaved { source_id: src.clone(), saved: report.new });
        }
    }

    // Previews for already-indexed photos (nothing to save in the database).
    if !cancel.load(Ordering::Relaxed) && !backfill.is_empty() {
        pool.install(|| {
            backfill.par_iter().for_each(|(rel, size, fp)| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let mut bytes = Vec::with_capacity(*size as usize);
                    // Same photo as indexed only: a changed file is re-indexed on the next scan.
                    let ok = *size <= MAX_PHOTO_BYTES
                        && source.open(rel).and_then(|f| Ok(f.take(*size + 1).read_to_end(&mut bytes)?)).is_ok()
                        && bytes.len() as u64 == *size
                        && fingerprint(&mut Cursor::new(&bytes[..]), *size).is_ok_and(|f| &f == fp);
                    if ok {
                        let _ = previews.store_from_original(fp, &bytes);
                    }
                }));
                read_backfill.fetch_add(1, Ordering::Relaxed);
            });
        });
    }

    report.cancelled = read_new.into_inner() < misses.len() as u64 || read_backfill.into_inner() < backfill.len() as u64;
    // Remember the folder only after a complete, clean scan, so anything missed is retried next time.
    if !report.cancelled && report.failed.is_empty() {
        let store = store.lock().unwrap();
        let signature = FolderSignature { photos: report.found, raw: report.skipped_unsupported, other: report.other_files, indexed: store.source_photo_count(&src)?, ..signature };
        store.mark_scanned(&src, &signature)?;
    }
    Ok(report)
}
