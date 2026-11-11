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
    /// Like `list`, but fails if any folder can't be read, instead of skipping it. An offload may only
    /// say "everything on the card is backed up" when it saw everything.
    fn list_complete(&self) -> Result<Vec<MediaEntry>> {
        self.list()
    }
    fn open(&self, rel_path: &str) -> Result<Box<dyn ReadSeek>>;
    /// Open for one front-to-back read of a big file (offload copies). Platforms may skip the OS
    /// cache so a whole card doesn't push everything else out of memory. Still read-only.
    fn open_streaming(&self, rel_path: &str) -> Result<Box<dyn ReadSeek>> {
        self.open(rel_path)
    }
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

/// Free space and filesystem of the backup drive.
#[derive(Debug, Clone)]
pub struct LibrarySpace {
    pub free_bytes: u64,
    /// e.g. "apfs", "exfat", "msdos".
    pub fs_type: String,
    /// The drive has a DCIM folder at its top, like a camera card (and so does the card): worth a warning.
    pub card_like: bool,
}

/// The backup drive an offload writes to. Paths are `/`-separated and relative to the folder the
/// user picked. Only adds files: never overwrites, moves or deletes anything except its own temp files.
pub trait Library: Send + Sync {
    /// The picked folder, for display.
    fn root_display(&self) -> String;
    fn space(&self) -> Result<LibrarySpace>;
    /// A small text file of ours (the library id), if it exists.
    fn read_text(&self, rel_path: &str) -> Option<String>;
    /// Create a new text file (and its folders). Fails if the name exists.
    fn create_text(&self, rel_path: &str, text: &str) -> Result<()>;
    /// Start writing a temp file next to `dest_rel` (folders created as needed). `tag` makes the temp name unique.
    fn begin(&self, dest_rel: &str, tag: &str) -> Result<Box<dyn PartialWrite>>;
    /// Make the drive write its own cache to the medium (macOS: `F_FULLFSYNC` through this file).
    fn flush_to_media(&self, rel_path: &str) -> Result<()>;
    /// Read the file back from the drive (bypassing the OS cache) and check its length and xxh3-128.
    fn verify(&self, rel_path: &str, size: u64, hash: u128) -> Result<()>;
    /// Give the verified temp file its final name, never replacing anything. Returns the file's mtime.
    fn commit(&self, partial_rel: &str, dest_rel: &str) -> Result<Option<SystemTime>>;
    /// Remove one of our own temp files (refuses any other name).
    fn discard(&self, partial_rel: &str) -> Result<()>;
}

/// A temp file being written by an offload.
pub trait PartialWrite: Send {
    fn rel_path(&self) -> &str;
    fn write(&mut self, buf: &[u8]) -> Result<()>;
    /// Flush it to the drive (`fsync`) and close it.
    fn finish(self: Box<Self>) -> Result<()>;
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CoreEvent {
    #[serde(rename_all = "camelCase")]
    ScanProgress { source_id: String, done: u64, total: u64 },
    /// A batch of new photos was saved: the library (days, counts, suggestions) can be refreshed mid-scan.
    #[serde(rename_all = "camelCase")]
    ScanSaved { source_id: String, saved: u64 },
    #[serde(rename_all = "camelCase")]
    ExportProgress { done: u64, total: u64 },
    #[serde(rename_all = "camelCase")]
    AnalysisProgress { done: u64, total: u64 },
    /// Offload: `phase` is "card" (reading the card to plan), "library" (checking the backup drive) or
    /// "copy". Copy counts files verified on the drive; bytes count what was written.
    #[serde(rename_all = "camelCase")]
    OffloadProgress { phase: String, done: u64, total: u64, bytes_done: u64, bytes_total: u64, current: String },
}

/// Makes the cached previews: JPEG in, smaller upright JPEGs out. Platforms with a fast native
/// codec provide their own (macOS/iOS: ImageIO, which decodes straight at a reduced size);
/// `preview::RustPreviewMaker` works everywhere.
pub trait PreviewMaker: Send + Sync {
    /// Stable id of this implementation, for logs and timings.
    fn implementation(&self) -> &'static str;
    /// One JPEG per entry of `edges` (largest first), in that order. Each has its longest edge at
    /// most that size (never upscaled) with the EXIF orientation applied. The input is decoded
    /// once; each smaller size is made from the one before it.
    fn render(&self, jpeg: &[u8], edges: &[u32]) -> Result<Vec<Vec<u8>>>;
}

/// Progress reporting, delivered to whatever UI exists.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: CoreEvent);
}

pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: CoreEvent) {}
}
