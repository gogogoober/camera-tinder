use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

pub type PhotoId = i64;
pub type SourceId = String;

/// The four swipe outcomes.
/// ← Dislike, → Like, ↑ Favorite, ↓ Hate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Like,
    Dislike,
    Favorite,
    Hate,
}

impl Decision {
    pub const ALL: [Decision; 4] = [Self::Like, Self::Dislike, Self::Favorite, Self::Hate];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::Favorite => "favorite",
            Self::Hate => "hate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }

    /// Photos worth keeping (exported). Dislike/Hate are tracked but never deleted.
    pub fn is_keeper(self) -> bool {
        matches!(self, Self::Like | Self::Favorite)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Jpeg,
    Raw,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Raw => "raw",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "jpeg" => Some(Self::Jpeg),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }

    /// Classify by extension. RAW is recognised (Nikon NEF, Fuji RAF, and
    /// common others) but not yet cullable; see `scan::CULLABLE_KINDS`.
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "nef" | "nrw" | "raf" | "cr2" | "cr3" | "arw" | "dng" | "orf" | "rw2" => Some(Self::Raw),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    pub id: PhotoId,
    /// Content-based identity; survives moving files between card and disk.
    pub fingerprint: String,
    pub source_id: SourceId,
    pub rel_path: String,
    pub file_name: String,
    pub kind: MediaKind,
    pub size: u64,
    /// EXIF capture time, falling back to file modified time.
    pub captured_at: Option<NaiveDateTime>,
    pub camera: Option<String>,
    pub decision: Option<Decision>,
    /// 0–100, higher is sharper. `None` until analysed. See `sharpness.rs`.
    pub sharpness: Option<u8>,
}

/// Photos scoring below this are suggested as likely rejects.
pub const SOFT_THRESHOLD: u8 = 22;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: SourceId,
    pub name: String,
    pub photo_count: u64,
}

/// How much disk the preview cache uses, and what blocks clearing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheStatus {
    pub files: u64,
    pub bytes: u64,
    /// Names of cards / external drives holding known photo sources that are plugged in
    /// right now (the platform decides what counts). Clearing is only allowed when this is
    /// empty, so rebuilding previews can't quietly start reading a card again.
    pub connected_sources: Vec<String>,
}

/// Counts per decision for a set of photos.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total: u64,
    pub undecided: u64,
    pub like: u64,
    pub dislike: u64,
    pub favorite: u64,
    pub hate: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySummary {
    /// `YYYY-MM-DD`, or `None` when the capture time is unknown.
    pub day: Option<String>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusFilter {
    #[default]
    Undecided,
    All,
    Keepers,
    Rejects,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    /// `YYYY-MM-DD` to restrict to one day (or the start of a range); `None` for all days.
    pub day: Option<String>,
    /// Inclusive end of a day range starting at `day`.
    #[serde(default)]
    pub day_to: Option<String>,
    #[serde(default)]
    pub status: StatusFilter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub found: u64,
    pub new: u64,
    /// RAW files: recognised, not yet cullable or exported.
    pub skipped_unsupported: u64,
    /// Everything else the app ignores (videos, audio memos, sidecars…). Not exported either.
    pub other_files: u64,
    pub failed: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub destination: String,
    pub copied: u64,
    pub already_there: u64,
    pub failed: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub include_likes: bool,
    pub include_favorites: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { include_likes: true, include_favorites: true }
    }
}

/// A half-open index range `[start, end)` into a plan's photo list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub start: usize,
    pub end: usize,
}

/// A queue of photos plus how they cluster in time.
/// `moments` split on gaps of minutes; `bursts` are runs shot seconds apart (size >= 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub photos: Vec<Photo>,
    pub moments: Vec<Group>,
    pub bursts: Vec<Group>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagSource {
    Manual,
    Calendar,
    Place,
    Gap,
    Image,
}

impl TagSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Calendar => "calendar",
            Self::Place => "place",
            Self::Gap => "gap",
            Self::Image => "image",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "calendar" => Self::Calendar,
            "place" => Self::Place,
            "gap" => Self::Gap,
            "image" => Self::Image,
            _ => Self::Manual,
        }
    }
}

/// A named run of days ("Lisbon", "Maya's wedding").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// `YYYY-MM-DD`, inclusive.
    pub start_day: String,
    pub end_day: String,
    pub source: TagSource,
}

/// A guess at a tag the user can accept or dismiss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagSuggestion {
    pub name: String,
    pub start_day: String,
    pub end_day: String,
    pub source: TagSource,
    pub reason: String,
    pub photo_count: u64,
}
