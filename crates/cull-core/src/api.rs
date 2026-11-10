//! `CullerService`: the one facade every UI talks to.

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use crate::error::{CoreError, Result};
use crate::model::*;
use crate::moments::{MOMENT_GAP_SECS, bursts, group_by_gap};
use crate::ports::{CalendarSource, EventSink, ExportItem, ExportTarget, MediaSource};
use crate::preview::PreviewCache;
use crate::scan::scan_source;
use crate::store::Store;
use crate::suggest;

const UNDO_DEPTH: usize = 500;

pub struct CullerService {
    store: Mutex<Store>,
    /// Sources in the current session (the folders/cards being culled).
    sources: RwLock<Vec<Arc<dyn MediaSource>>>,
    previews: PreviewCache,
    /// One entry per user action; a bulk decision is a single entry.
    undo: Mutex<Vec<Vec<(PhotoId, Option<Decision>)>>>,
    calendar: RwLock<Option<Arc<dyn CalendarSource>>>,
}

impl CullerService {
    pub fn open(db_path: &Path, cache_dir: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::with_store(Store::open(db_path)?, cache_dir)
    }

    pub fn in_memory(cache_dir: &Path) -> Result<Self> {
        Self::with_store(Store::open_in_memory()?, cache_dir)
    }

    fn with_store(store: Store, cache_dir: &Path) -> Result<Self> {
        Ok(Self {
            store: Mutex::new(store),
            sources: RwLock::new(Vec::new()),
            previews: PreviewCache::new(cache_dir.to_path_buf())?,
            undo: Mutex::new(Vec::new()),
            calendar: RwLock::new(None),
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

    /// Scan every source in the session. Safe to call repeatedly: unchanged
    /// files are skipped and previous decisions are kept.
    pub fn scan(&self, sink: &dyn EventSink) -> Result<ScanReport> {
        let sources = self.sources.read().unwrap().clone();
        let mut total = ScanReport::default();
        for s in sources {
            let r = scan_source(&self.store, s.as_ref(), &self.previews, sink)?;
            total.found += r.found;
            total.new += r.new;
            total.skipped_unsupported += r.skipped_unsupported;
            total.other_files += r.other_files;
            total.failed.extend(r.failed);
        }
        Ok(total)
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
    pub fn suggest_tags(&self) -> Result<Vec<TagSuggestion>> {
        let days = self.days()?;
        let existing = self.tags()?;
        let mut out = Vec::new();
        let dated: Vec<&str> = days.iter().filter_map(|d| d.day.as_deref()).collect();
        if let (Some(first), Some(last), Some(cal)) = (dated.first(), dated.last(), self.calendar.read().unwrap().clone()) {
            out.extend(suggest::from_calendar(&days, &cal.events(first, last)?, &existing));
        }
        out.extend(suggest::from_gaps(&days, &existing));
        Ok(out)
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
