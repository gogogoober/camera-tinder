//! `CullerService`: the one facade every UI talks to.

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::{EventSink, ExportItem, ExportTarget, MediaSource};
use crate::preview::PreviewCache;
use crate::scan::scan_source;
use crate::store::Store;

const UNDO_DEPTH: usize = 500;

pub struct CullerService {
    store: Mutex<Store>,
    /// Sources in the current session (the folders/cards being culled).
    sources: RwLock<Vec<Arc<dyn MediaSource>>>,
    previews: PreviewCache,
    undo: Mutex<Vec<(PhotoId, Option<Decision>)>>,
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
            let r = scan_source(&self.store, s.as_ref(), sink)?;
            total.found += r.found;
            total.new += r.new;
            total.skipped_unsupported += r.skipped_unsupported;
            total.failed.extend(r.failed);
        }
        Ok(total)
    }

    pub fn queue(&self, filter: &Filter) -> Result<Vec<Photo>> {
        self.store.lock().unwrap().query(&self.source_ids(), filter)
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
        let source = self.source(&photo.source_id)?;
        self.previews.get(&photo, source.as_ref(), max_edge)
    }

    // ---- deciding -----------------------------------------------------

    pub fn decide(&self, id: PhotoId, decision: Decision) -> Result<()> {
        let previous = self.store.lock().unwrap().set_decision(id, Some(decision))?;
        let mut undo = self.undo.lock().unwrap();
        undo.push((id, previous));
        if undo.len() > UNDO_DEPTH {
            undo.remove(0);
        }
        Ok(())
    }

    /// Revert the most recent decision. Returns the photo so the UI can show it again.
    pub fn undo(&self) -> Result<Option<Photo>> {
        let Some((id, previous)) = self.undo.lock().unwrap().pop() else {
            return Ok(None);
        };
        self.store.lock().unwrap().set_decision(id, previous)?;
        Ok(Some(self.photo(id)?))
    }

    // ---- export -------------------------------------------------------

    pub fn keepers(&self, opts: ExportOptions) -> Result<Vec<Photo>> {
        let photos = self.queue(&Filter { day: None, status: StatusFilter::Keepers })?;
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
