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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: SourceId,
    pub name: String,
    pub photo_count: u64,
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
    /// `YYYY-MM-DD` to restrict to one day; `None` for all days.
    pub day: Option<String>,
    #[serde(default)]
    pub status: StatusFilter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub found: u64,
    pub new: u64,
    pub skipped_unsupported: u64,
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
