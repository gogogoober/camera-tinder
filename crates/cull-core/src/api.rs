//! `CullerService`: the one facade every UI talks to.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, TryLockError};

use rayon::prelude::*;

use crate::analysis::grouping::{Candidate, similar_groups};
use crate::analysis::pipeline::{Pipeline, StepResult, run_photo, stale_steps};
use crate::analysis::issues::issues;
use crate::analysis::{
    AnalysisImage, AnalysisKind, AnalyzeReport, AnalyzeRequest, AnalyzerInfo, Analyzers, GroupingOptions, PhotoAnalysis, PhotoIssues, SimilarGroup,
};
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::moments::{MOMENT_GAP_SECS, group_by_gap};
use crate::ports::{CalendarSource, EventSink, ExportItem, ExportTarget, MediaSource};
use crate::preview::{MASTER_EDGE, PreviewCache};
use crate::scan::scan_source;
use crate::store::Store;
use crate::suggest;

const UNDO_DEPTH: usize = 500;
/// Photos analysed at once. Each holds a decoded 2048 px master (~12 MB) while it runs.
const ANALYZE_THREADS: usize = 4;
/// Photos per progress event / database transaction.
const ANALYZE_BATCH: usize = 16;

/// Wakes background analysis whenever the scan has saved a batch of photos.
struct WakeOnSaved<'a> {
    inner: &'a dyn EventSink,
    svc: &'a CullerService,
}
impl EventSink for WakeOnSaved<'_> {
    fn emit(&self, event: crate::ports::CoreEvent) {
        if matches!(event, crate::ports::CoreEvent::ScanSaved { .. }) {
            self.svc.wake_analysis();
        }
        self.inner.emit(event);
    }
}

/// One photo's analysis in a batch: its results, or why it couldn't be analysed.
type PhotoSteps = std::result::Result<(PhotoId, Vec<StepResult>), (PhotoId, String)>;

pub struct CullerService {
    store: Mutex<Store>,
    /// The app's own folders (database folder, preview cache): never scanned as photos.
    app_dirs: Vec<std::path::PathBuf>,
    /// Sources in the current session (the folders/cards being culled).
    sources: RwLock<Vec<Arc<dyn MediaSource>>>,
    previews: PreviewCache,
    /// One entry per user action; a bulk decision is a single entry.
    undo: Mutex<Vec<Vec<(PhotoId, Option<Decision>)>>>,
    calendar: RwLock<Option<Arc<dyn CalendarSource>>>,
    analyzers: RwLock<Arc<Analyzers>>,
    /// Where background errors go (see `errlog.rs`); stderr only until the platform sets a file.
    error_log: RwLock<Option<Arc<crate::errlog::ErrorLog>>>,
    /// Background analysis: wake signal and the Stop flag (see `worker.rs`).
    pub(crate) analysis_control: Arc<crate::worker::AnalysisControl>,
    /// One scan at a time.
    scanning: Mutex<()>,
    /// Set by `cancel_scan`; the running scan stops after the photos it is reading.
    scan_cancel: AtomicBool,
}

impl CullerService {
    pub fn open(db_path: &Path, cache_dir: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut svc = Self::with_store(Store::open(db_path)?, cache_dir)?;
        svc.app_dirs.extend(db_path.parent().map(Path::to_path_buf));
        Ok(svc)
    }

    pub fn in_memory(cache_dir: &Path) -> Result<Self> {
        Self::with_store(Store::open_in_memory()?, cache_dir)
    }

    fn with_store(store: Store, cache_dir: &Path) -> Result<Self> {
        Ok(Self {
            store: Mutex::new(store),
            app_dirs: vec![cache_dir.to_path_buf()],
            sources: RwLock::new(Vec::new()),
            previews: PreviewCache::new(cache_dir.to_path_buf())?,
            undo: Mutex::new(Vec::new()),
            calendar: RwLock::new(None),
            analyzers: RwLock::new(Arc::new(Analyzers::rust())),
            analysis_control: crate::worker::AnalysisControl::new(),
            error_log: RwLock::new(None),
            scanning: Mutex::new(()),
            scan_cancel: AtomicBool::new(false),
        })
    }

    // ---- sources -------------------------------------------------------

    pub fn add_source(&self, source: Arc<dyn MediaSource>) -> Result<SourceInfo> {
        self.store.lock().unwrap().upsert_source(source.id(), source.display_name())?;
        let mut sources = self.sources.write().unwrap();
        sources.retain(|s| s.id() != source.id());
        sources.push(source.clone());
        drop(sources);
        // Photos scanned in an earlier session may still need analysis.
        self.wake_analysis();
        self.source_info(source.as_ref())
    }

    /// The app's own folders (database, preview cache). Platforms keep sources from listing them.
    pub fn app_dirs(&self) -> &[std::path::PathBuf] {
        &self.app_dirs
    }

    pub fn remove_source(&self, id: &str) {
        self.sources.write().unwrap().retain(|s| s.id() != id);
    }

    /// Ids of every source ever added, in this session or before. Export targets use
    /// these to refuse writing into a photo source.
    pub fn known_source_ids(&self) -> Result<Vec<String>> {
        let mut ids = self.store.lock().unwrap().all_source_ids()?;
        for s in self.sources.read().unwrap().iter() {
            if !ids.iter().any(|i| i == s.id()) {
                ids.push(s.id().to_string());
            }
        }
        Ok(ids)
    }

    pub fn sources(&self) -> Result<Vec<SourceInfo>> {
        let sources = self.sources.read().unwrap().clone();
        sources.iter().map(|s| self.source_info(s.as_ref())).collect()
    }

    fn source_info(&self, s: &dyn MediaSource) -> Result<SourceInfo> {
        Ok(SourceInfo {
            id: s.id().to_string(),
            name: s.display_name().to_string(),
            photo_count: self.store.lock().unwrap().source_photo_count(s.id())?,
        })
    }

    fn source_ids(&self) -> Vec<String> {
        self.sources.read().unwrap().iter().map(|s| s.id().to_string()).collect()
    }

    fn source(&self, id: &str) -> Result<Arc<dyn MediaSource>> {
        self.sources
            .read()
            .unwrap()
            .iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or_else(|| CoreError::UnknownSource(id.to_string()))
    }

    // ---- scanning & browsing ------------------------------------------

    /// Scan the requested sources (all of them by default). Safe to call repeatedly:
    /// files already indexed are not read again and previous decisions are kept.
    /// New photos are saved (and `ScanSaved` emitted) in batches while it runs.
    pub fn scan(&self, req: &ScanRequest, sink: &dyn EventSink) -> Result<ScanReport> {
        // A scan that crashed must not block every later one.
        let _one_at_a_time = self.scanning.lock().unwrap_or_else(PoisonError::into_inner);
        self.scan_cancel.store(false, Ordering::Relaxed);
        let sources: Vec<_> = self
            .sources
            .read()
            .unwrap()
            .iter()
            .filter(|s| req.source_ids.as_ref().is_none_or(|ids| ids.iter().any(|id| id == s.id())))
            .cloned()
            .collect();
        let sink = &WakeOnSaved { inner: sink, svc: self };
        let mut total = ScanReport::default();
        for s in sources {
            if self.scan_cancel.load(Ordering::Relaxed) {
                total.cancelled = true;
                break;
            }
            let r = scan_source(&self.store, s.as_ref(), &self.previews, &self.scan_cancel, sink)?;
            total.found += r.found;
            total.new += r.new;
            total.skipped_unsupported += r.skipped_unsupported;
            total.other_files += r.other_files;
            total.failed.extend(r.failed);
            total.cancelled |= r.cancelled;
        }
        // Previews backfilled at the end may have made more photos analysable.
        self.wake_analysis();
        Ok(total)
    }

    /// Stop the running scan (if any) after the photos it is reading right now.
    /// Everything already processed is kept; the next scan picks up the rest.
    pub fn cancel_scan(&self) {
        self.scan_cancel.store(true, Ordering::Relaxed);
    }

    pub fn queue(&self, filter: &Filter) -> Result<Vec<Photo>> {
        self.store.lock().unwrap().query(&self.source_ids(), filter)
    }

    /// The queue plus how it clusters in time (moments) and what to ask about each photo (triage).
    pub fn plan(&self, filter: &Filter) -> Result<Plan> {
        let photos = self.queue(filter)?;
        let times: Vec<_> = photos.iter().map(|p| p.captured_at).collect();
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let analyzers = self.analyzers.read().unwrap().clone();
        let mut analyses = self.store.lock().unwrap().load_analyses(&ids, None)?;
        analyses.values_mut().for_each(|a| a.retain_current(&analyzers));
        let triage = crate::triage::build(&photos, &analyses);
        Ok(Plan {
            moments: group_by_gap(&times, MOMENT_GAP_SECS),
            triage,
            triage_version: crate::triage::PLAN_VERSION.to_string(),
            photos,
        })
    }

    /// Forget decisions and/or analysis for the photos taken on `req.days` in this session's folders
    /// and cards (another card with photos from the same days is left alone), so those days can be
    /// triaged or analysed again from scratch (mainly for testing). Database only: photos, previews and
    /// the original files are never touched. An empty `days` does nothing.
    pub fn reset_days(&self, req: &ResetRequest) -> Result<ResetReport> {
        for day in &req.days {
            // Exactly YYYY-MM-DD (chrono alone accepts "2026-7-1", which would silently match no photo).
            let canonical = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").map(|d| d.format("%Y-%m-%d").to_string());
            if day != "unknown" && canonical.as_deref() != Ok(day.as_str()) {
                return Err(CoreError::Other(format!("not a day (YYYY-MM-DD or \"unknown\"): {day:?}")));
            }
        }
        if req.days.is_empty() || !(req.decisions || req.analysis) {
            return Ok(ResetReport::default());
        }
        let mut store = self.store.lock().unwrap();
        // Only the session's folders and cards: exactly the photos the triage screen shows for these days.
        let ids = store.photo_ids_on_days(&self.source_ids(), &req.days)?;
        let report = store.transaction(|s| {
            Ok(ResetReport {
                photos: ids.len() as u64,
                decisions: if req.decisions { s.forget_decisions(&ids)? } else { 0 },
                analysis: if req.analysis { s.forget_analysis(&ids)? } else { 0 },
            })
        })?;
        drop(store);
        if req.decisions {
            // Undo must not bring back what was just forgotten.
            self.undo.lock().unwrap().clear();
        }
        if req.analysis {
            self.wake_analysis(); // does nothing while analysis is stopped
        }
        Ok(report)
    }

    /// Record decisions made on suggested photos (see `triage::OutcomeEntry`). Call it after `decide`.
    pub fn log_outcomes(&self, entries: &[crate::triage::OutcomeEntry]) -> Result<()> {
        self.store.lock().unwrap().transaction(|s| s.insert_outcomes(entries))
    }

    pub fn days(&self) -> Result<Vec<DaySummary>> {
        self.store.lock().unwrap().days(&self.source_ids())
    }

    pub fn stats(&self) -> Result<Stats> {
        self.store.lock().unwrap().stats(&self.source_ids())
    }

    pub fn photo(&self, id: PhotoId) -> Result<Photo> {
        self.store.lock().unwrap().photo(id)
    }

    /// Display-sized JPEG for a photo (cached).
    pub fn preview(&self, id: PhotoId, max_edge: u32) -> Result<Vec<u8>> {
        let photo = self.photo(id)?;
        // Normally served from the cache built at scan time; the source is only a
        // fallback for photos indexed by older versions (and may be unplugged).
        let source = self.source(&photo.source_id).ok();
        self.previews.get(&photo, source.as_deref(), max_edge)
    }

    /// Preview cache size, plus which known sources block clearing right now.
    /// `is_connected(source_id)` is the platform's answer (desktop: "is it on a plugged-in
    /// card or external drive?").
    pub fn preview_cache_status(&self, is_connected: &dyn Fn(&str) -> bool) -> Result<PreviewCacheStatus> {
        let (files, bytes) = self.previews.usage()?;
        let mut known = self.store.lock().unwrap().all_sources()?;
        for s in self.sources.read().unwrap().iter() {
            if !known.iter().any(|(id, _)| id == s.id()) {
                known.push((s.id().to_string(), s.display_name().to_string()));
            }
        }
        let connected_sources = known.into_iter().filter(|(id, _)| is_connected(id)).map(|(_, name)| name).collect();
        Ok(PreviewCacheStatus { files, bytes, connected_sources })
    }

    /// Delete the app's own preview copies and return the bytes freed. Refused while any
    /// card / external source is connected: with them gone, nothing can trigger a re-read of
    /// the originals. Never touches photos, the database (decisions, index) or any file
    /// the cache didn't create; a later scan rebuilds previews.
    pub fn clear_previews(&self, is_connected: &dyn Fn(&str) -> bool) -> Result<u64> {
        // Not while a scan runs: it would delete previews the scan has just made.
        let _no_scan: MutexGuard<()> = match self.scanning.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(CoreError::Other("a scan is running; stop it or let it finish first".into())),
        };
        let status = self.preview_cache_status(is_connected)?;
        if !status.connected_sources.is_empty() {
            return Err(CoreError::Other(format!("disconnect these first: {}", status.connected_sources.join(", "))));
        }
        self.previews.clear()
    }

    // ---- deciding -----------------------------------------------------

    pub fn decide(&self, id: PhotoId, decision: Decision) -> Result<()> {
        self.decide_many(&[id], decision)
    }

    /// Give several photos the same decision as one undoable action.
    pub fn decide_many(&self, ids: &[PhotoId], decision: Decision) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let items: Vec<_> = ids.iter().map(|id| (*id, Some(decision))).collect();
        let previous = self.store.lock().unwrap().set_many(&items)?;
        let mut undo = self.undo.lock().unwrap();
        undo.push(ids.iter().copied().zip(previous).collect());
        if undo.len() > UNDO_DEPTH {
            undo.remove(0);
        }
        Ok(())
    }

    /// Revert the most recent action (one photo or a whole batch). Returns the photos
    /// so the UI can show them again; empty if there was nothing to undo.
    pub fn undo(&self) -> Result<Vec<Photo>> {
        let Some(batch) = self.undo.lock().unwrap().pop() else {
            return Ok(vec![]);
        };
        self.store.lock().unwrap().set_many(&batch)?;
        batch.iter().map(|(id, _)| self.photo(*id)).collect()
    }

    // ---- suggestions --------------------------------------------------

    /// Platforms with a calendar (EventKit) plug it in here.
    pub fn set_calendar(&self, source: Option<Arc<dyn CalendarSource>>) {
        *self.calendar.write().unwrap() = source;
    }

    /// Groups of days worth offering: calendar events and runs of days from capture times.
    /// `dismissed` holds row ids (`TagSuggestion::id`) to leave out; the rest is regrouped.
    pub fn suggest_tags(&self, dismissed: &[String]) -> Result<Vec<TagSuggestion>> {
        let days = self.days()?;
        let mut out = Vec::new();
        let dated: Vec<&str> = days.iter().filter_map(|d| d.day.as_deref()).collect();
        if let (Some(first), Some(last), Some(cal)) = (dated.first(), dated.last(), self.calendar.read().unwrap().clone()) {
            out.extend(suggest::from_calendar(&days, &cal.events(first, last)?, dismissed));
        }
        out.extend(suggest::from_gaps(&days, dismissed));
        Ok(out)
    }

    // ---- photo analysis ---------------------------------------------
    // See docs/photo-analysis.md. Analysis reads the cached master previews, never the originals.

    /// Platforms with a faster native codec replace the pure-Rust preview maker here
    /// (e.g. `cull_platform_apple::imageio::ImageIoPreviewMaker`).
    pub fn set_preview_maker(&self, maker: Arc<dyn crate::ports::PreviewMaker>) {
        self.previews.set_maker(maker);
    }

    /// Platforms point background error logging at a file in the app's own log folder.
    pub fn set_error_log(&self, path: std::path::PathBuf) {
        *self.error_log.write().unwrap() = Some(Arc::new(crate::errlog::ErrorLog::new(path)));
    }

    /// Log a background error (stderr, plus the error log file when one is set).
    pub(crate) fn log_error(&self, area: &str, message: &str) {
        match self.error_log.read().unwrap().as_ref() {
            Some(log) => log.record(area, message),
            None => eprintln!("[{area}] {message}"),
        }
    }

    /// Like `log_error`, once per `key` per app session (a photo failing on every pass logs once).
    pub(crate) fn log_error_once(&self, key: &str, area: &str, message: &str) {
        match self.error_log.read().unwrap().as_ref() {
            Some(log) => log.record_once(key, area, message),
            None => eprintln!("[{area}] {message}"),
        }
    }

    /// Where the error log is, and its last `max_lines` lines (oldest first). For a developer view.
    pub fn error_log(&self, max_lines: usize) -> ErrorLogView {
        match self.error_log.read().unwrap().as_ref() {
            Some(log) => ErrorLogView { path: Some(log.path().display().to_string()), lines: log.recent(max_lines) },
            None => ErrorLogView::default(),
        }
    }

    /// Platforms add engines here (e.g. `cull_platform_apple::vision::analyzers()`).
    /// Defaults to `Analyzers::rust()`.
    pub fn set_analyzers(&self, analyzers: Analyzers) {
        *self.analyzers.write().unwrap() = Arc::new(analyzers);
    }

    /// Which implementation serves each analysis kind on this platform.
    pub fn analyzer_info(&self) -> Vec<AnalyzerInfo> {
        self.analyzers.read().unwrap().info()
    }

    fn photos_for(&self, ids: Option<&[PhotoId]>, filter: Option<&Filter>) -> Result<Vec<Photo>> {
        match (ids, filter) {
            (Some(ids), _) => ids.iter().map(|id| self.photo(*id)).collect(),
            (None, Some(f)) => self.queue(f),
            (None, None) => self.queue(&Filter { status: StatusFilter::All, ..Default::default() }),
        }
    }

    /// Compute (and store) analysis results. Only missing or stale results are computed
    /// unless `force`. Blocking and CPU-heavy: call it off the UI thread.
    ///
    /// Returns early (keeping what's saved) while analysis is stopped (`stop_analysis`).
    ///
    /// Reads only the cached master previews, never the originals: a photo without one is
    /// reported in `failed` (a later scan rebuilds the preview).
    pub fn analyze(&self, req: &AnalyzeRequest, sink: &dyn EventSink) -> Result<AnalyzeReport> {
        let pipeline = Pipeline::from_analyzers(self.analyzers.read().unwrap().clone()).only(&req.kinds);
        let photos = self.photos_for(req.photo_ids.as_deref(), req.filter.as_ref())?;
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let mut report = AnalyzeReport { photos: photos.len() as u64, ..Default::default() };

        let index = self.store.lock().unwrap().result_index(&ids)?;
        let todo: Vec<(Photo, Vec<usize>)> = photos
            .into_iter()
            .filter_map(|p| {
                let stale = stale_steps(&pipeline, index.get(&p.id), req.force);
                (!stale.is_empty()).then_some((p, stale))
            })
            .collect();
        report.up_to_date = report.photos - todo.len() as u64;

        let total = todo.len() as u64;
        let mut done = 0u64;
        sink.emit(crate::ports::CoreEvent::AnalysisProgress { done, total });
        let pool = rayon::ThreadPoolBuilder::new().num_threads(ANALYZE_THREADS).build().map_err(|e| CoreError::Other(e.to_string()))?;
        let stop = &self.analysis_control.stop;
        for batch in todo.chunks(ANALYZE_BATCH) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let results: Vec<PhotoSteps> = pool.install(|| {
                batch
                    .par_iter()
                    .map(|(photo, steps)| {
                        if stop.load(Ordering::Relaxed) {
                            return Ok((photo.id, Vec::new()));
                        }
                        // No source: a photo without a cached master is skipped, never read from the card.
                        let bytes = self.previews.get(photo, None, MASTER_EDGE).map_err(|e| (photo.id, e.to_string()))?;
                        let img = AnalysisImage::from_jpeg(bytes);
                        // A decoder or engine bug on one damaged photo must not end the pass (or the worker).
                        std::panic::catch_unwind(AssertUnwindSafe(|| run_photo(&pipeline, steps, photo.id, &img, stop).0))
                            .map(|results| (photo.id, results))
                            .map_err(|_| (photo.id, "analysis crashed on this photo".to_string()))
                    })
                    .collect()
            });
            let mut store = self.store.lock().unwrap();
            store.transaction(|s| {
                for r in &results {
                    match r {
                        Ok((_, step_results)) if step_results.is_empty() => {} // stopped before this photo
                        Ok((id, step_results)) => {
                            report.kind_errors += step_results.iter().filter(|x| x.result.is_err()).count() as u64;
                            report.analyzed += 1;
                            s.save_results(*id, step_results)?;
                        }
                        Err(f) => report.failed.push(f.clone()),
                    }
                }
                Ok(())
            })?;
            drop(store);
            self.log_photo_failures(batch, &results);
            done += batch.len() as u64;
            sink.emit(crate::ports::CoreEvent::AnalysisProgress { done, total });
        }
        Ok(report)
    }

    /// Log each photo that couldn't be analysed, and each engine error, once per app session.
    fn log_photo_failures(&self, batch: &[(Photo, Vec<usize>)], results: &[PhotoSteps]) {
        let name = |id: PhotoId| batch.iter().find(|(p, _)| p.id == id).map(|(p, _)| p.file_name.as_str()).unwrap_or("?");
        for r in results {
            match r {
                Err((id, why)) => self.log_error_once(&format!("photo:{id}"), "analysis", &format!("photo {id} ({}) not analysed: {why}", name(*id))),
                Ok((id, steps)) => {
                    for s in steps {
                        if let Err(why) = &s.result {
                            self.log_error_once(&format!("photo:{id}:{}", s.kind), "analysis", &format!("photo {id} ({}): {} failed: {why}", name(*id), s.kind));
                        }
                    }
                }
            }
        }
    }

    /// Everything stored for one photo, including its similarity embedding.
    pub fn photo_analysis(&self, id: PhotoId) -> Result<PhotoAnalysis> {
        let mut a = self.store.lock().unwrap().load_analyses(&[id], None)?.remove(&id).unwrap_or_else(|| PhotoAnalysis::new(id));
        a.retain_current(&self.analyzers.read().unwrap());
        Ok(a)
    }

    /// Stored analyses for the photos matching `filter`, in queue order, without the
    /// (large) similarity embeddings.
    pub fn analyses(&self, filter: &Filter) -> Result<Vec<PhotoAnalysis>> {
        let photos = self.queue(filter)?;
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let kinds: Vec<AnalysisKind> = AnalysisKind::ALL.into_iter().filter(|k| *k != AnalysisKind::Similarity).collect();
        let mut map = self.store.lock().unwrap().load_analyses(&ids, Some(&kinds))?;
        let analyzers = self.analyzers.read().unwrap().clone();
        Ok(ids
            .iter()
            .filter_map(|id| map.remove(id))
            .map(|mut a| {
                a.retain_current(&analyzers);
                a
            })
            .collect())
    }

    /// Reject reasons for the photos matching `filter` (only photos with at least one), in
    /// queue order. Powers "problem" views: blurry, eyes closed, dark, …
    pub fn photo_issues(&self, filter: &Filter) -> Result<Vec<PhotoIssues>> {
        Ok(self
            .analyses(filter)?
            .iter()
            .map(|a| PhotoIssues { photo_id: a.photo_id, issues: issues(a) })
            .filter(|p| !p.issues.is_empty())
            .collect())
    }

    /// Near-duplicate groups among the photos matching `filter`, each ordered best-first.
    /// Needs `perceptualHash` and/or `similarity` results (run `analyze` first).
    pub fn similar_groups(&self, filter: &Filter, options: &GroupingOptions) -> Result<Vec<SimilarGroup>> {
        let photos = self.queue(filter)?;
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let mut map = self.store.lock().unwrap().load_analyses(&ids, None)?;
        let analyzers = self.analyzers.read().unwrap().clone();
        map.values_mut().for_each(|a| a.retain_current(&analyzers));
        let candidates: Vec<Candidate> = photos.iter().filter_map(|p| Some(Candidate { captured_at: p.captured_at, analysis: map.get(&p.id)? })).collect();
        Ok(similar_groups(&candidates, options))
    }

    // ---- export -------------------------------------------------------

    /// The photos `opts` selects: the chosen decisions, optionally only on some days.
    pub fn keepers(&self, opts: ExportOptions) -> Result<Vec<Photo>> {
        let photos = self.queue(&Filter { days: opts.days.clone(), status: StatusFilter::All })?;
        let exported = if opts.include_exported {
            Default::default()
        } else {
            self.store.lock().unwrap().exported_ids(&photos.iter().map(|p| p.id).collect::<Vec<_>>())?
        };
        Ok(photos
            .into_iter()
            .filter(|p| !exported.contains(&p.id))
            .filter(|p| match p.decision {
                Some(Decision::Like) => opts.include_likes,
                Some(Decision::Favorite) => opts.include_favorites,
                Some(Decision::Dislike) => opts.include_dislikes,
                Some(Decision::Hate) => opts.include_hates,
                None => false,
            })
            .collect())
    }

    /// How many photos `export` would copy for `opts`.
    pub fn export_count(&self, opts: ExportOptions) -> Result<usize> {
        Ok(self.keepers(opts)?.len())
    }

    // ---- saved tags ---------------------------------------------------

    pub fn list_tags(&self) -> Result<Vec<SavedTag>> {
        self.store.lock().unwrap().list_tags()
    }

    /// Create the tag `name` or replace the days of the existing one. Days must be `YYYY-MM-DD` or `"unknown"`.
    pub fn save_tag(&self, name: &str, days: &[String]) -> Result<SavedTag> {
        if let Some(bad) = days.iter().find(|d| *d != "unknown" && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err()) {
            return Err(CoreError::Other(format!("not a day: {bad}")));
        }
        let mut days = days.to_vec();
        days.sort();
        days.dedup();
        self.store.lock().unwrap().save_tag(name, &days)
    }

    pub fn delete_tag(&self, id: i64) -> Result<()> {
        self.store.lock().unwrap().delete_tag(id)
    }

    pub fn export(&self, target: &dyn ExportTarget, opts: ExportOptions, sink: &dyn EventSink) -> Result<ExportReport> {
        let photos = self.keepers(opts)?;
        let sources = self.sources.read().unwrap().clone();
        // Keepers are only listed for sources in this session. If one of those has been
        // unplugged, `open` fails and the copy is reported in `failed`, never dropped silently.
        let items: Vec<ExportItem> = photos
            .iter()
            .filter_map(|p| {
                let source = sources.iter().find(|s| s.id() == p.source_id)?;
                Some(ExportItem { photo: p, source: source.as_ref() })
            })
            .collect();
        let mut report = target.export(&items, sink)?;
        // Remember what is now in the destination, so a later export can skip it. The copies are done
        // either way, so a failure here is reported alongside them, never instead of them.
        if let Err(e) = self.store.lock().unwrap().mark_exported(&report.delivered, &report.destination) {
            self.log_error("export", &format!("copied {} photos to {} but could not mark them exported: {e}", report.delivered.len(), report.destination));
            report.not_marked = Some(e.to_string());
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::pipeline::{Pipeline, run_photo};
    use crate::analysis::{Embedding, PerceptualHash};
    use crate::ports::{MediaEntry, ReadSeek};
    use crate::store::NewPhoto;
    use chrono::NaiveDateTime;

    /// A source with no files: it only has to exist so its photos belong to the session.
    struct EmptySource;
    impl MediaSource for EmptySource {
        fn id(&self) -> &str {
            "s"
        }
        fn display_name(&self) -> &str {
            "s"
        }
        fn list(&self) -> Result<Vec<MediaEntry>> {
            Ok(vec![])
        }
        fn open(&self, _: &str) -> Result<Box<dyn ReadSeek>> {
            Err(CoreError::Other("empty".into()))
        }
    }

    fn seeded(tmp: &std::path::Path, days: &[&str]) -> (CullerService, Vec<PhotoId>) {
        let svc = CullerService::in_memory(tmp).unwrap();
        svc.add_source(Arc::new(EmptySource)).unwrap();
        let img = AnalysisImage::from_image(::image::DynamicImage::ImageRgb8(::image::ImageBuffer::from_fn(64, 64, |x, y| ::image::Rgb([x as u8 * 4, y as u8 * 4, 90]))));
        let pipeline = Pipeline::from_analyzers(svc.analyzers.read().unwrap().clone());
        let results = run_photo(&pipeline, &(0..pipeline.steps.len()).collect::<Vec<_>>(), 0, &img, &AtomicBool::new(false)).0;
        let ids = days
            .iter()
            .enumerate()
            .map(|(n, day)| {
                let fp = format!("fp{n}");
                let when = NaiveDateTime::parse_from_str(&format!("{day}T10:00:{n:02}"), "%Y-%m-%dT%H:%M:%S").unwrap();
                let store = svc.store.lock().unwrap();
                let (id, _) = store.upsert_photo(&NewPhoto { fingerprint: &fp, source_id: "s", rel_path: &fp, kind: MediaKind::Jpeg, size: 1, captured_at: Some(when), camera: None }).unwrap();
                store.save_results(id, &results).unwrap();
                id
            })
            .collect();
        (svc, ids)
    }

    #[test]
    fn reset_days_forgets_only_those_days_and_undo_cannot_bring_them_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (svc, ids) = seeded(tmp.path(), &["2026-07-01", "2026-07-01", "2026-07-02"]);
        svc.decide_many(&ids, Decision::Favorite).unwrap();
        // A second card with a photo from the same day, scanned earlier but not in this session.
        let other = {
            let store = svc.store.lock().unwrap();
            store.upsert_source("other-card", "other").unwrap();
            let when = NaiveDateTime::parse_from_str("2026-07-01T12:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
            let (id, _) = store.upsert_photo(&NewPhoto { fingerprint: "other", source_id: "other-card", rel_path: "o.jpg", kind: MediaKind::Jpeg, size: 1, captured_at: Some(when), camera: None }).unwrap();
            id
        };
        svc.store.lock().unwrap().set_decision(other, Some(Decision::Like)).unwrap();

        let r = svc.reset_days(&ResetRequest { days: vec!["2026-07-01".into()], decisions: true, analysis: false }).unwrap();
        assert_eq!((r.photos, r.decisions, r.analysis), (2, 2, 0));
        let decided: Vec<_> = ids.iter().map(|id| svc.photo(*id).unwrap().decision).collect();
        assert_eq!(decided, [None, None, Some(Decision::Favorite)]);
        assert_eq!(svc.photo(other).unwrap().decision, Some(Decision::Like), "a card not in the session is left alone");
        assert!(svc.undo().unwrap().is_empty(), "undo can't resurrect a reset");
        assert!(svc.photo_analysis(ids[0]).unwrap().blur.is_some(), "analysis kept unless asked");

        let r = svc.reset_days(&ResetRequest { days: vec!["2026-07-02".into()], decisions: false, analysis: true }).unwrap();
        assert!(r.analysis > 0);
        assert!(svc.photo_analysis(ids[2]).unwrap().blur.is_none());
        assert_eq!(svc.photo(ids[2]).unwrap().decision, Some(Decision::Favorite), "decisions kept unless asked");
    }

    #[test]
    fn reset_days_never_means_everything_and_rejects_bad_days() {
        let tmp = tempfile::tempdir().unwrap();
        let (svc, ids) = seeded(tmp.path(), &["2026-07-01"]);
        svc.decide(ids[0], Decision::Like).unwrap();
        let none = svc.reset_days(&ResetRequest { days: vec![], decisions: true, analysis: true }).unwrap();
        assert_eq!(none, ResetReport::default(), "an empty list resets nothing");
        let nothing_chosen = svc.reset_days(&ResetRequest { days: vec!["2026-07-01".into()], decisions: false, analysis: false }).unwrap();
        assert_eq!(nothing_chosen, ResetReport::default());
        for bad in ["2026-7-1", "July 1", "", "2026-07-01T10:00:00", "%"] {
            assert!(svc.reset_days(&ResetRequest { days: vec![bad.into()], decisions: true, analysis: true }).is_err(), "{bad:?}");
        }
        assert_eq!(svc.photo(ids[0]).unwrap().decision, Some(Decision::Like), "nothing was touched");
        assert!(svc.photo_analysis(ids[0]).unwrap().blur.is_some());
    }

    /// The triage plan is computed on every request instead of being stored, so it has to stay fast
    /// at the size of a real session: 2,000 fully analysed photos over 5 days, a third of them in bursts.
    /// If this ever fails, cache the plan; don't loosen the limit silently.
    #[test]
    fn a_triage_plan_for_2000_analysed_photos_is_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = CullerService::in_memory(tmp.path()).unwrap();
        svc.add_source(Arc::new(EmptySource)).unwrap();

        // Real results for one tiny image, reused for every photo with its own hash and embedding.
        let img = AnalysisImage::from_image(::image::DynamicImage::ImageRgb8(::image::ImageBuffer::from_fn(64, 64, |x, y| ::image::Rgb([x as u8 * 4, y as u8 * 4, 90]))));
        let pipeline = Pipeline::from_analyzers(svc.analyzers.read().unwrap().clone());
        let template = run_photo(&pipeline, &(0..pipeline.steps.len()).collect::<Vec<_>>(), 0, &img, &AtomicBool::new(false)).0;

        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let mut scene_vector: Vec<f32> = Vec::new();
        let mut when = NaiveDateTime::parse_from_str("2026-01-01T08:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        {
            let mut store = svc.store.lock().unwrap();
            store
                .transaction(|s| {
                    for n in 0..2000 {
                        // Every third photo starts a new scene minutes later; the others follow a second after.
                        when += chrono::Duration::seconds(if n % 3 == 0 { 120 + (next() % 600) as i64 } else { 1 });
                        let fp = format!("fp{n}");
                        let (id, _) = s.upsert_photo(&NewPhoto { fingerprint: &fp, source_id: "s", rel_path: &fp, kind: MediaKind::Jpeg, size: 1, captured_at: Some(when), camera: None })?;
                        let mut results = template.clone();
                        for r in &mut results {
                            if r.kind == "perceptualHash" {
                                // Frames of one scene share a hash; scenes differ.
                                r.result = Ok(serde_json::to_value(PerceptualHash(if n % 3 == 0 { next() } else { 0 })).unwrap());
                            }
                            if r.kind == "similarity" {
                                // Frames of one scene share an embedding too; a new scene gets a fresh one.
                                if n % 3 == 0 {
                                    scene_vector = (0..64).map(|_| (next() % 1000) as f32 / 1000.0).collect();
                                }
                                let vector = scene_vector.clone();
                                r.result = Ok(serde_json::to_value(Embedding { model: r.implementation.clone(), vector }).unwrap());
                            }
                        }
                        s.save_results(id, &results)?;
                    }
                    Ok(())
                })
                .unwrap();
        }

        let start = std::time::Instant::now();
        let plan = svc.plan(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
        let took = start.elapsed();
        println!("plan() for 2000 analysed photos: {took:?}");
        assert_eq!(plan.triage.len(), 2000);
        assert!(plan.triage.iter().all(|t| t.analyzed));
        assert!(plan.triage.iter().any(|t| t.question == crate::triage::Question::Pick), "bursts were found");
        assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
    }
}
