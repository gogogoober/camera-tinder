//! Running an offload. Per file: read the card once (hashing as it streams), write a temp file, fsync.
//! Per batch: one flush of the drive's own cache, then read every temp file back from the drive, check
//! it, give it its final name (never overwriting), flush again, and only then record it as backed up.
//!
//! Three threads overlap the work: the card reader, the writer (this thread), and the verifier, so the
//! card keeps streaming while the previous batch is read back from the drive.

use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use xxhash_rust::xxh3::Xxh3;

use super::journal::{CachedFile, ItemStart};
use super::kind::FileKind;
use super::plan::{CountByType, FailedFile, LIBRARY_ID_FILE, OWN_DIR, PlanInput, PlannedFile, SPACE_MARGIN};
use crate::error::{CoreError, Result};
use crate::identity::StreamingFingerprint;
use crate::ports::{CoreEvent, EventSink, Library, MediaSource, PartialWrite};
use crate::store::Store;

const CHUNK: usize = 4 << 20;
/// Chunks in flight between the card reader and the writer (16 × 4 MiB).
const QUEUE: usize = 16;
const BATCH_FILES: usize = 200;
const BATCH_BYTES: u64 = 2 << 30;
/// Read or write failures in a row before the run gives up (card or drive unplugged).
const FAILURES_IN_A_ROW: u32 = 3;
const STOPPED: &str = "stopped";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OffloadReport {
    pub library_root: String,
    pub copied: u64,
    pub bytes: u64,
    /// Card files that were already on the drive.
    pub already: u64,
    pub failed: Vec<FailedFile>,
    /// Photos and videos on the card that are neither verified on the drive nor already there.
    pub not_backed_up: Vec<String>,
    /// Set when the card couldn't be listed again at the end (unplugged).
    pub recheck_error: Option<String>,
    pub skipped: Vec<CountByType>,
    pub stopped: bool,
    /// Why the run gave up early (card or drive gone, drive full).
    pub error: Option<String>,
    /// The hash list written on the drive, relative to the picked folder.
    pub manifest: Option<String>,
    pub seconds: f64,
    /// Every photo and video on the card is on the drive, read back and matched.
    pub all_safe: bool,
    /// Where the time went: busy time and bytes of each step (they overlap, so the slowest one sets the pace).
    pub stages: StageTimes,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageTimes {
    /// Reading the card.
    pub read_bytes: u64,
    pub read_seconds: f64,
    /// Writing to the drive (including the per-file fsync).
    pub write_bytes: u64,
    pub write_seconds: f64,
    /// Reading copies back from the drive and checking them.
    pub check_bytes: u64,
    pub check_seconds: f64,
}

/// Adds the time `f` takes to `ns`.
fn timed<T>(ns: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    out
}

pub struct RunArgs<'a> {
    pub input: &'a PlanInput,
    pub planned: &'a [PlannedFile],
    pub card: &'a dyn MediaSource,
    pub lib: &'a dyn Library,
    pub store: &'a Mutex<Store>,
    pub sink: &'a dyn EventSink,
    pub stop: &'a AtomicBool,
}

enum Msg {
    Begin(usize),
    Chunk(Vec<u8>),
    End { hash: u128, fingerprint: String, len: u64 },
    Failed(String),
}

struct Written {
    /// Index into `planned`.
    p: usize,
    partial: String,
    hash: u128,
}

struct Shared<'a> {
    a: &'a RunArgs<'a>,
    run_id: i64,
    library_id: String,
    /// Card or drive gone: everything stops.
    abort: AtomicBool,
    fatal: Mutex<Option<String>>,
    failed: Mutex<Vec<FailedFile>>,
    verified: Mutex<Vec<(usize, u128)>>,
    bytes_written: AtomicU64,
    last_emit: Mutex<Instant>,
    read_bytes: AtomicU64,
    read_ns: AtomicU64,
    write_ns: AtomicU64,
    check_bytes: AtomicU64,
    check_ns: AtomicU64,
}

impl Shared<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.a.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn halted(&self) -> bool {
        self.a.stop.load(Ordering::Relaxed) || self.abort.load(Ordering::Relaxed)
    }

    fn give_up(&self, why: String) {
        self.fatal.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(why);
        self.abort.store(true, Ordering::Relaxed);
    }

    fn fail(&self, p: usize, error: &str, partial: Option<&str>) {
        let src = &self.a.input.files[self.a.planned[p].index].rel_path;
        let gone = partial.is_none_or(|rel| self.a.lib.discard(rel).is_ok());
        let _ = self.lock().offload_item_failed(self.run_id, src, error, gone);
        if error != STOPPED {
            self.failed.lock().unwrap_or_else(PoisonError::into_inner).push(FailedFile { path: src.clone(), error: error.to_string() });
        }
    }

    fn emit(&self, current: &str, force: bool) {
        let mut last = self.last_emit.lock().unwrap_or_else(PoisonError::into_inner);
        if !force && last.elapsed() < Duration::from_millis(200) {
            return;
        }
        *last = Instant::now();
        let planned = self.a.planned;
        self.a.sink.emit(CoreEvent::OffloadProgress {
            phase: "copy".into(),
            done: self.verified.lock().unwrap_or_else(PoisonError::into_inner).len() as u64,
            total: planned.len() as u64,
            bytes_done: self.bytes_written.load(Ordering::Relaxed),
            bytes_total: planned.iter().map(|p| self.a.input.files[p.index].size).sum(),
            current: current.to_string(),
        });
    }
}

fn new_library_id(root: &str) -> String {
    let mut h = Xxh3::new();
    h.update(root.as_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().to_le_bytes());
    format!("{:032x}", h.digest128())
}

fn is_disk_full(e: &CoreError) -> bool {
    matches!(e, CoreError::Io(io) if io.kind() == std::io::ErrorKind::StorageFull || io.raw_os_error() == Some(28))
}

/// Read until `buf` is full or the file ends.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub fn run(a: RunArgs<'_>) -> Result<OffloadReport> {
    let started = Instant::now();
    let input = a.input;
    let total_bytes: u64 = a.planned.iter().map(|p| input.files[p.index].size).sum();
    let space = a.lib.space()?;
    if total_bytes > 0 && total_bytes + SPACE_MARGIN > space.free_bytes {
        return Err(CoreError::Other(format!(
            "not enough space on the drive: {} MB needed (plus 1 GB to spare), {} MB free",
            total_bytes >> 20,
            space.free_bytes >> 20
        )));
    }

    // The drive's id lives on the drive, so it is recognised whatever it's mounted as.
    let id_rel = format!("{}{LIBRARY_ID_FILE}", input.prefix);
    let library_id = match a.lib.read_text(&id_rel).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => {
            let id = new_library_id(&input.library_root);
            match a.lib.create_text(&id_rel, &format!("{id}\n")) {
                Ok(()) => id,
                Err(e) => a.lib.read_text(&id_rel).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or(e)?,
            }
        }
    };

    // Temp files a crashed or unplugged earlier run left on this drive.
    let run_id = {
        let store = a.store.lock().unwrap_or_else(PoisonError::into_inner);
        store.offload_close_stale_runs()?;
        for (run, src, partial) in store.offload_leftover_partials(&library_id)? {
            if a.lib.discard(&partial).is_ok() {
                store.offload_partial_cleaned(run, &src)?;
            }
        }
        store.offload_start_run(&library_id, &input.library_root, &input.card_root)?
    };

    let sh = Shared {
        a: &a,
        run_id,
        library_id: library_id.clone(),
        abort: AtomicBool::new(false),
        fatal: Mutex::new(None),
        failed: Mutex::new(Vec::new()),
        verified: Mutex::new(Vec::new()),
        bytes_written: AtomicU64::new(0),
        last_emit: Mutex::new(Instant::now() - Duration::from_secs(1)),
        read_bytes: AtomicU64::new(0),
        read_ns: AtomicU64::new(0),
        write_ns: AtomicU64::new(0),
        check_bytes: AtomicU64::new(0),
        check_ns: AtomicU64::new(0),
    };
    sh.emit("", true);

    std::thread::scope(|s| {
        let (tx, rx) = sync_channel::<Msg>(QUEUE);
        let (btx, brx) = sync_channel::<Vec<Written>>(1);
        let sh = &sh;
        s.spawn(move || read_card(sh, tx));
        let verifier = s.spawn(move || verify_batches(sh, brx));
        write_files(sh, rx, &btx);
        drop(btx);
        if verifier.join().is_err() {
            sh.give_up("the verifier crashed; files not recorded as verified stay unconfirmed".into());
        }
    });

    let secs = |ns: &AtomicU64| ns.load(Ordering::Relaxed) as f64 / 1e9;
    let stages = StageTimes {
        read_bytes: sh.read_bytes.load(Ordering::Relaxed),
        read_seconds: secs(&sh.read_ns),
        write_bytes: sh.bytes_written.load(Ordering::Relaxed),
        write_seconds: secs(&sh.write_ns),
        check_bytes: sh.check_bytes.load(Ordering::Relaxed),
        check_seconds: secs(&sh.check_ns),
    };

    // Re-list the card: every photo and video on it must be on the drive.
    let verified = sh.verified.into_inner().unwrap_or_else(PoisonError::into_inner);
    let mut covered: HashSet<(&str, u64)> =
        input.files.iter().filter(|f| f.on_drive.is_some()).map(|f| (f.rel_path.as_str(), f.size)).collect();
    covered.extend(verified.iter().map(|(p, _)| {
        let f = &input.files[a.planned[*p].index];
        (f.rel_path.as_str(), f.size)
    }));
    let (not_backed_up, recheck_error) = match a.card.list_complete() {
        Ok(list) => (
            list.into_iter()
                .filter(|e| FileKind::of(&e.rel_path).is_some() && !covered.contains(&(e.rel_path.as_str(), e.size)))
                .map(|e| e.rel_path)
                .collect::<Vec<_>>(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };

    // A plain-text hash list next to the photos, to re-check the drive later without the app.
    let mut manifest = None;
    if !verified.is_empty() {
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
        let rel = format!("{}{OWN_DIR}/offload-{stamp}.txt", input.prefix);
        let mut text = format!(
            "# Darkroom offload {stamp}\n# card: {}\n# xxh3-128  size  path (relative to {})\n",
            input.card_root,
            if input.prefix.is_empty() { "this folder".into() } else { input.prefix.trim_end_matches('/').to_string() }
        );
        for (p, hash) in &verified {
            let pf = &a.planned[*p];
            let path = pf.dest_rel.strip_prefix(&input.prefix).unwrap_or(&pf.dest_rel);
            text.push_str(&format!("{hash:032x}  {}  {path}\n", input.files[pf.index].size));
        }
        if a.lib.create_text(&rel, &text).is_ok() {
            manifest = Some(rel);
        }
    }

    let failed = sh.failed.into_inner().unwrap_or_else(PoisonError::into_inner);
    let error = sh.fatal.into_inner().unwrap_or_else(PoisonError::into_inner);
    let stopped = a.stop.load(Ordering::Relaxed);
    let copied = verified.len() as u64;
    let bytes: u64 = verified.iter().map(|(p, _)| input.files[a.planned[*p].index].size).sum();
    let status = if error.is_some() {
        "failed"
    } else if stopped {
        "stopped"
    } else {
        "done"
    };
    a.store.lock().unwrap_or_else(PoisonError::into_inner).offload_finish_run(run_id, status, copied, bytes)?;
    let all_safe = not_backed_up.is_empty() && recheck_error.is_none() && failed.is_empty() && error.is_none() && !stopped;
    Ok(OffloadReport {
        library_root: input.library_root.clone(),
        copied,
        bytes,
        already: input.files.iter().filter(|f| f.on_drive.is_some()).count() as u64,
        failed,
        not_backed_up,
        recheck_error,
        skipped: input.skipped.iter().map(|(e, n)| CountByType { extension: e.clone(), count: *n }).collect(),
        stopped,
        error,
        manifest,
        seconds: started.elapsed().as_secs_f64(),
        all_safe,
        stages,
    })
}

/// The card, front to back, one file at a time. Never reads past the listed size (+1 byte to notice growth).
fn read_card(sh: &Shared<'_>, tx: SyncSender<Msg>) {
    let mut fails = 0;
    for (p, pf) in sh.a.planned.iter().enumerate() {
        if sh.halted() || tx.send(Msg::Begin(p)).is_err() {
            return;
        }
        let f = &sh.a.input.files[pf.index];
        let res = (|| -> std::result::Result<(u128, String, u64), String> {
            let r = sh.a.card.open_streaming(&f.rel_path).map_err(|e| e.to_string())?;
            let mut r = r.take(f.size + 1);
            let mut hash = Xxh3::new();
            let mut fp = StreamingFingerprint::new(f.size);
            let mut len = 0u64;
            loop {
                if sh.halted() {
                    return Err(STOPPED.into());
                }
                let mut buf = vec![0u8; CHUNK];
                let n = timed(&sh.read_ns, || read_full(&mut r, &mut buf)).map_err(|e| e.to_string())?;
                sh.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
                if n == 0 {
                    break;
                }
                buf.resize(n, 0); // shrink to what was read
                hash.update(&buf);
                fp.update(&buf);
                len += n as u64;
                tx.send(Msg::Chunk(buf)).map_err(|_| STOPPED.to_string())?;
            }
            Ok((hash.digest128(), fp.finish(), len))
        })();
        match res {
            Ok((hash, fingerprint, len)) => {
                fails = 0;
                if tx.send(Msg::End { hash, fingerprint, len }).is_err() {
                    return;
                }
            }
            Err(e) => {
                let stopped = e == STOPPED;
                let _ = tx.send(Msg::Failed(e.clone()));
                if stopped {
                    return;
                }
                fails += 1;
                if fails >= FAILURES_IN_A_ROW {
                    sh.give_up(format!("the card stopped responding ({e})"));
                    return;
                }
            }
        }
    }
}

/// Write each file to a temp file on the drive; hand finished batches to the verifier.
fn write_files(sh: &Shared<'_>, rx: Receiver<Msg>, btx: &SyncSender<Vec<Written>>) {
    let a = sh.a;
    let mut cur: Option<(usize, Box<dyn PartialWrite>)> = None;
    let mut batch: Vec<Written> = Vec::new();
    let mut batch_bytes = 0u64;
    let fails = std::cell::Cell::new(0u32);
    let on_error = |sh: &Shared<'_>, e: &CoreError| {
        fails.set(fails.get() + 1);
        if is_disk_full(e) {
            sh.give_up("the drive is full".into());
        } else if fails.get() >= FAILURES_IN_A_ROW {
            sh.give_up(format!("the drive stopped accepting files ({e})"));
        }
    };
    let send = |batch: &mut Vec<Written>, batch_bytes: &mut u64| {
        if batch.is_empty() {
            return;
        }
        // One flush of the drive's cache covers every file of the batch (each was already fsynced).
        if let Err(e) = a.lib.flush_to_media(&batch.last().expect("not empty").partial) {
            for w in batch.drain(..) {
                sh.fail(w.p, &format!("couldn't flush the drive: {e}"), Some(&w.partial));
            }
        } else if let Err(std::sync::mpsc::SendError(rest)) = btx.send(std::mem::take(batch)) {
            for w in rest {
                sh.fail(w.p, "verifier stopped", Some(&w.partial));
            }
        }
        *batch_bytes = 0;
    };

    for msg in rx {
        match msg {
            Msg::Begin(p) => {
                let pf = &a.planned[p];
                let f = &a.input.files[pf.index];
                // Before each batch: room for at least this file (plus 64 MB), or stop cleanly.
                if batch.is_empty() && a.lib.space().is_ok_and(|s| s.free_bytes < f.size + (64 << 20)) {
                    sh.give_up("the drive is full".into());
                }
                if sh.halted() {
                    sh.fail(p, STOPPED, None);
                    cur = None;
                    continue;
                }
                // Unique across runs and app copies (two databases can both have a run 1).
                match a.lib.begin(&pf.dest_rel, &format!("{}-{}-{p}", std::process::id(), sh.run_id)) {
                    Ok(w) => {
                        let started = sh.lock().offload_item_started(
                            sh.run_id,
                            &ItemStart {
                                src_rel: &f.rel_path,
                                size: f.size,
                                fingerprint: &f.fingerprint,
                                partial_rel: w.rel_path(),
                                dest_rel: &pf.dest_rel,
                                brand: &pf.brand,
                                year: pf.year,
                                folder_key: &pf.folder_key,
                                day: f.day().map(|d| d.to_string()),
                            },
                        );
                        if let Err(e) = started {
                            let partial = w.rel_path().to_string();
                            drop(w);
                            let _ = a.lib.discard(&partial);
                            sh.give_up(format!("couldn't record progress in the database ({e})"));
                            cur = None;
                        } else {
                            cur = Some((p, w));
                        }
                    }
                    Err(e) => {
                        sh.failed.lock().unwrap_or_else(PoisonError::into_inner).push(FailedFile { path: f.rel_path.clone(), error: e.to_string() });
                        on_error(sh, &e);
                        cur = None;
                    }
                }
            }
            Msg::Chunk(buf) => {
                if let Some((p, w)) = cur.as_mut() {
                    match timed(&sh.write_ns, || w.write(&buf)) {
                        Ok(()) => {
                            sh.bytes_written.fetch_add(buf.len() as u64, Ordering::Relaxed);
                            sh.emit(&a.input.files[a.planned[*p].index].rel_path, false);
                        }
                        Err(e) => {
                            let (p, w) = cur.take().expect("current file");
                            let partial = w.rel_path().to_string();
                            drop(w);
                            sh.fail(p, &e.to_string(), Some(&partial));
                            on_error(sh, &e);
                        }
                    }
                }
            }
            Msg::End { hash, fingerprint, len } => {
                let Some((p, w)) = cur.take() else { continue };
                let f = &a.input.files[a.planned[p].index];
                let partial = w.rel_path().to_string();
                if len != f.size || fingerprint != f.fingerprint {
                    drop(w);
                    sh.fail(p, "the file on the card changed since the plan was made; plan again", Some(&partial));
                    continue;
                }
                match timed(&sh.write_ns, || w.finish(f.modified)) {
                    Ok(()) => {
                        fails.set(0);
                        batch.push(Written { p, partial, hash });
                        batch_bytes += len;
                        if batch.len() >= BATCH_FILES || batch_bytes >= BATCH_BYTES {
                            send(&mut batch, &mut batch_bytes);
                        }
                    }
                    Err(e) => {
                        sh.fail(p, &e.to_string(), Some(&partial));
                        on_error(sh, &e);
                    }
                }
            }
            Msg::Failed(e) => {
                if let Some((p, w)) = cur.take() {
                    let partial = w.rel_path().to_string();
                    drop(w);
                    sh.fail(p, &e, Some(&partial));
                }
            }
        }
    }
    if let Some((p, w)) = cur.take() {
        let partial = w.rel_path().to_string();
        drop(w);
        sh.fail(p, STOPPED, Some(&partial));
    }
    send(&mut batch, &mut batch_bytes);
}

/// Read each temp file back from the drive, check it, name it, flush, and only then record it.
fn verify_batches(sh: &Shared<'_>, brx: Receiver<Vec<Written>>) {
    let a = sh.a;
    for batch in brx {
        let mut named = Vec::new();
        for w in batch {
            let pf = &a.planned[w.p];
            let f = &a.input.files[pf.index];
            let checked = timed(&sh.check_ns, || a.lib.verify(&w.partial, f.size, w.hash));
            if checked.is_ok() {
                sh.check_bytes.fetch_add(f.size, Ordering::Relaxed);
            }
            match checked {
                Err(e) => sh.fail(w.p, &format!("the copy doesn't match the card: {e}"), Some(&w.partial)),
                Ok(()) => match a.lib.commit(&w.partial, &pf.dest_rel) {
                    Ok(mtime) => named.push((w, mtime)),
                    Err(e) => sh.fail(w.p, &e.to_string(), Some(&w.partial)),
                },
            }
            sh.emit(&f.rel_path, false);
        }
        let Some((last, _)) = named.last() else { continue };
        // Final names are on the drive's medium before anything is recorded as backed up.
        if let Err(e) = a.lib.flush_to_media(&a.planned[last.p].dest_rel) {
            for (w, _) in named {
                sh.fail(w.p, &format!("copied, but the drive couldn't confirm it was saved: {e}"), None);
            }
            continue;
        }
        let mut store = sh.lock();
        let mut cache = Vec::new();
        let mut folders = HashSet::new();
        for (w, mtime) in &named {
            let pf = &a.planned[w.p];
            let f = &a.input.files[pf.index];
            if store.offload_item_verified(sh.run_id, &f.rel_path, w.hash).is_err() {
                continue;
            }
            cache.push((
                pf.dest_rel.clone(),
                CachedFile {
                    size: f.size,
                    mtime: mtime.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
                    fingerprint: f.fingerprint.clone(),
                },
            ));
            if folders.insert((pf.brand.clone(), pf.year, pf.folder_key.clone())) {
                let _ = store.offload_remember_folder(&sh.library_id, &pf.brand, pf.year, &pf.folder_key, &pf.folder_rel);
            }
            sh.verified.lock().unwrap_or_else(PoisonError::into_inner).push((w.p, w.hash));
        }
        let _ = store.offload_cache_files(&super::plan::cache_key_for(&sh.library_id, &a.input.prefix), &cache);
        drop(store);
        sh.emit(&a.input.files[a.planned[last.p].index].rel_path, true);
    }
}
