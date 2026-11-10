//! The seams between the core and each platform.
//!
//! - Desktop: `cull-platform-fs` (plain filesystem paths, copy-to-folder).
//! - iOS (later): a security-scoped / ImageCaptureCore `MediaSource` and a
//!   share-sheet `ExportTarget`, implemented natively and bridged in.

use std::io::{Read, Seek};
use std::time::SystemTime;

use serde::Serialize;

use crate::error::Result;
use crate::model::{ExportReport, Photo};

pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[derive(Debug, Clone)]
pub struct MediaEntry {
    /// Path relative to the source root, always `/`-separated.
    pub rel_path: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Somewhere photos come from: a folder, an SD card, a camera.
pub trait MediaSource: Send + Sync {
    /// Stable identifier (e.g. canonical root path).
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn list(&self) -> Result<Vec<MediaEntry>>;
    fn open(&self, rel_path: &str) -> Result<Box<dyn ReadSeek>>;
}

/// Where calendar events come from (macOS/iOS EventKit later). Optional.
pub trait CalendarSource: Send + Sync {
    /// Events overlapping `from..=to` (`YYYY-MM-DD`), reduced to the days they cover.
    fn events(&self, from: &str, to: &str) -> Result<Vec<crate::suggest::CalendarEvent>>;
}

pub struct ExportItem<'a> {
    pub photo: &'a Photo,
    pub source: &'a dyn MediaSource,
}

/// Somewhere keepers go: a folder (v1), Lightroom, the iOS share sheet...
pub trait ExportTarget: Send + Sync {
    fn name(&self) -> &str;
    fn export(&self, items: &[ExportItem<'_>], sink: &dyn EventSink) -> Result<ExportReport>;
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CoreEvent {
    #[serde(rename_all = "camelCase")]
    ScanProgress { source_id: String, done: u64, total: u64 },
    #[serde(rename_all = "camelCase")]
    ExportProgress { done: u64, total: u64 },
}

/// Progress reporting, delivered to whatever UI exists.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: CoreEvent);
}

pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: CoreEvent) {}
}
