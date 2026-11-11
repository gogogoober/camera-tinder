//! Which card files get backed up, and where in a folder each kind goes.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Jpeg,
    Raw,
    Heif,
    Video,
}

impl FileKind {
    /// By extension. `None` = not a photo or video: never copied (proxies, thumbnails, XML, sidecars…).
    pub fn of(name: &str) -> Option<Self> {
        let ext = extension(name)?;
        Some(match ext.as_str() {
            "jpg" | "jpeg" => Self::Jpeg,
            "nef" | "nrw" | "raf" | "dng" | "cr2" | "cr3" | "arw" | "orf" | "rw2" | "pef" => Self::Raw,
            "hif" | "heic" | "heif" => Self::Heif,
            "mov" | "mp4" | "m4v" | "avi" | "mts" | "m2ts" | "nev" => Self::Video,
            _ => return None,
        })
    }

    /// Subfolder inside a trip or month folder; JPEGs sit in the folder itself.
    pub fn subfolder(self) -> Option<&'static str> {
        match self {
            Self::Jpeg => None,
            Self::Raw => Some("RAW"),
            Self::Heif => Some("HIF"),
            Self::Video => Some("Videos"),
        }
    }

    /// Kinds whose EXIF names the camera and the capture time. Videos use the file's time instead.
    pub fn has_exif(self) -> bool {
        self != Self::Video
    }
}

/// Lower-case extension without the dot.
pub fn extension(name: &str) -> Option<String> {
    let file = name.rsplit('/').next().unwrap_or(name);
    let (stem, ext) = file.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(FileKind::of("DCIM/100_FUJI/DSCF0001.JPG"), Some(FileKind::Jpeg));
        assert_eq!(FileKind::of("DSCF0001.RAF"), Some(FileKind::Raw));
        assert_eq!(FileKind::of("DSC_0001.nef"), Some(FileKind::Raw));
        assert_eq!(FileKind::of("DSCF0001.HIF"), Some(FileKind::Heif));
        assert_eq!(FileKind::of("DSCF0002.MOV"), Some(FileKind::Video));
        assert_eq!(FileKind::of("DSC_0003.NEV"), Some(FileKind::Video));
        for skipped in ["DSCF0002.LRF", "DSCF0002.THM", "M4ROOT/CLIP.XML", "photo.xmp", "noext", ".JPG"] {
            assert_eq!(FileKind::of(skipped), None, "{skipped}");
        }
        assert_eq!(FileKind::Raw.subfolder(), Some("RAW"));
        assert_eq!(FileKind::Jpeg.subfolder(), None);
    }
}
