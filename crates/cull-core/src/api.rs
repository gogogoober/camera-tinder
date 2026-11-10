//! `CullerService`: the one facade every UI talks to.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, TryLockError};

use rayon::prelude::*;

use crate::analysis::grouping::{Candidate, similar_groups};
use crate::analysis::issues::issues;
use crate::analysis::{
    AnalysisImage, AnalysisKind, AnalyzeReport, AnalyzeRequest, AnalyzerInfo, Analyzers, GroupingOptions, PhotoAnalysis, PhotoIssues, SimilarGroup,
};
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::moments::{MOMENT_GAP_SECS, bursts, group_by_gap};
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

    /// The queue plus how it clusters in time (moments and bursts), for the cull screen.
    pub fn plan(&self, filter: &Filter) -> Result<Plan> {
        let photos = self.queue(filter)?;
        let times: Vec<_> = photos.iter().map(|p| p.captured_at).collect();
        Ok(Plan { moments: group_by_gap(&times, MOMENT_GAP_SECS), bursts: bursts(&times), photos })
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
    /// the originals. Never touches photos, the database (decisions, tags, index) or any file
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

    // ---- tags ---------------------------------------------------------

    pub fn tags(&self) -> Result<Vec<Tag>> {
        self.store.lock().unwrap().tags()
    }

    pub fn add_tag(&self, name: &str, start_day: &str, end_day: &str, source: TagSource) -> Result<Tag> {
        self.store.lock().unwrap().add_tag(name.trim(), start_day, end_day, source)
    }

    pub fn remove_tag(&self, id: i64) -> Result<()> {
        self.store.lock().unwrap().remove_tag(id)
    }

    /// Platforms with a calendar (EventKit) plug it in here.
    pub fn set_calendar(&self, source: Option<Arc<dyn CalendarSource>>) {
        *self.calendar.write().unwrap() = source;
    }

    /// Tags worth offering: calendar events and runs of days from capture times.
    /// `dismissed` holds row ids (`TagSuggestion::id`) to leave out; the rest is regrouped.
    pub fn suggest_tags(&self, dismissed: &[String]) -> Result<Vec<TagSuggestion>> {
        let days = self.days()?;
        let existing = self.tags()?;
        let mut out = Vec::new();
        let dated: Vec<&str> = days.iter().filter_map(|d| d.day.as_deref()).collect();
        if let (Some(first), Some(last), Some(cal)) = (dated.first(), dated.last(), self.calendar.read().unwrap().clone()) {
            out.extend(suggest::from_calendar(&days, &cal.events(first, last)?, &existing, dismissed));
        }
        out.extend(suggest::from_gaps(&days, &existing, dismissed));
        Ok(out)
    }

    // ---- photo analysis ---------------------------------------------
    // See docs/photo-analysis.md. Analysis reads the cached master previews, never the originals.

    /// Platforms with a faster native codec replace the pure-Rust preview maker here
    /// (e.g. `cull_platform_apple::imageio::ImageIoPreviewMaker`).
    pub fn set_preview_maker(&self, maker: Arc<dyn crate::ports::PreviewMaker>) {
        self.previews.set_maker(maker);
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
    pub fn analyze(&self, req: &AnalyzeRequest, sink: &dyn EventSink) -> Result<AnalyzeReport> {
        let analyzers = self.analyzers.read().unwrap().clone();
        let available = analyzers.available();
        let kinds: Vec<AnalysisKind> = if req.kinds.is_empty() { available.clone() } else { req.kinds.iter().copied().filter(|k| available.contains(k)).collect() };
        let photos = self.photos_for(req.photo_ids.as_deref(), req.filter.as_ref())?;
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let mut report = AnalyzeReport { photos: photos.len() as u64, ..Default::default() };

        let index = self.store.lock().unwrap().analysis_index(&ids)?;
        let todo: Vec<(Photo, Vec<AnalysisKind>)> = photos
            .into_iter()
            .filter_map(|p| {
                let stored = index.get(&p.id);
                let stale: Vec<AnalysisKind> = kinds
                    .iter()
                    .copied()
                    .filter(|k| req.force || stored.and_then(|s| s.get(k)).map(String::as_str) != analyzers.implementation(*k))
                    .collect();
                (!stale.is_empty()).then_some((p, stale))
            })
            .collect();
        report.up_to_date = report.photos - todo.len() as u64;

        let total = todo.len() as u64;
        let mut done = 0u64;
        sink.emit(crate::ports::CoreEvent::AnalysisProgress { done, total });
        let pool = rayon::ThreadPoolBuilder::new().num_threads(ANALYZE_THREADS).build().map_err(|e| CoreError::Other(e.to_string()))?;
        let input_kinds = [AnalysisKind::Faces, AnalysisKind::Saliency];
        for batch in todo.chunks(ANALYZE_BATCH) {
            let batch_ids: Vec<PhotoId> = batch.iter().map(|(p, _)| p.id).collect();
            // Stored inputs (faces, saliency) so dependants don't recompute them.
            let prior = if req.force { Default::default() } else { self.store.lock().unwrap().load_analyses(&batch_ids, Some(&input_kinds))? };
            let results: Vec<std::result::Result<PhotoAnalysis, (PhotoId, String)>> = pool.install(|| {
                batch
                    .par_iter()
                    .map(|(photo, kinds)| {
                        let source = self.source(&photo.source_id).ok();
                        let bytes = self.previews.get(photo, source.as_deref(), MASTER_EDGE).map_err(|e| (photo.id, e.to_string()))?;
                        let img = AnalysisImage::from_jpeg(bytes);
                        Ok(analyzers.run(photo.id, &img, kinds, prior.get(&photo.id)))
                    })
                    .collect()
            });
            let mut store = self.store.lock().unwrap();
            store.transaction(|s| {
                for r in &results {
                    match r {
                        Ok(a) => {
                            report.kind_errors += a.errors.len() as u64;
                            report.analyzed += 1;
                            s.save_analysis(a)?;
                        }
                        Err(f) => report.failed.push(f.clone()),
                    }
                }
                Ok(())
            })?;
            drop(store);
            done += batch.len() as u64;
            sink.emit(crate::ports::CoreEvent::AnalysisProgress { done, total });
        }
        Ok(report)
    }

    /// Everything stored for one photo, including its similarity embedding.
    pub fn photo_analysis(&self, id: PhotoId) -> Result<PhotoAnalysis> {
        Ok(self.store.lock().unwrap().load_analyses(&[id], None)?.remove(&id).unwrap_or_else(|| PhotoAnalysis::new(id)))
    }

    /// Stored analyses for the photos matching `filter`, in queue order, without the
    /// (large) similarity embeddings.
    pub fn analyses(&self, filter: &Filter) -> Result<Vec<PhotoAnalysis>> {
        let photos = self.queue(filter)?;
        let ids: Vec<PhotoId> = photos.iter().map(|p| p.id).collect();
        let kinds: Vec<AnalysisKind> = AnalysisKind::ALL.into_iter().filter(|k| *k != AnalysisKind::Similarity).collect();
        let mut map = self.store.lock().unwrap().load_analyses(&ids, Some(&kinds))?;
        Ok(ids.iter().filter_map(|id| map.remove(id)).collect())
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
        let map = self.store.lock().unwrap().load_analyses(&ids, None)?;
        let candidates: Vec<Candidate> = photos.iter().filter_map(|p| Some(Candidate { captured_at: p.captured_at, analysis: map.get(&p.id)? })).collect();
        Ok(similar_groups(&candidates, options))
    }

    // ---- export -------------------------------------------------------

    pub fn keepers(&self, opts: ExportOptions) -> Result<Vec<Photo>> {
        let photos = self.queue(&Filter { day: None, day_to: None, status: StatusFilter::Keepers })?;
        Ok(photos
            .into_iter()
            .filter(|p| match p.decision {
                Some(Decision::Like) => opts.include_likes,
                Some(Decision::Favorite) => opts.include_favorites,
                _ => false,
            })
            .collect())
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
        target.export(&items, sink)
    }
}
