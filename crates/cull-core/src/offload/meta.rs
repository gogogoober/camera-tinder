//! Camera brand and capture time of any card file (JPEG, RAW incl. Fuji RAF, HEIF; videos fall back).

use std::io::{BufReader, Cursor, Read, SeekFrom};

use super::kind::FileKind;
use crate::metadata::{PhotoMeta, read_meta};
use crate::ports::ReadSeek;

/// What a file's EXIF says (both may be missing).
pub fn read_file_meta(kind: FileKind, name: &str, r: &mut dyn ReadSeek) -> PhotoMeta {
    if !kind.has_exif() {
        return PhotoMeta::default();
    }
    if super::kind::extension(name).as_deref() == Some("raf") {
        return read_raf(r).unwrap_or_default();
    }
    if kind == FileKind::Raw {
        // TIFF-based RAW (NEF, DNG, ARW…): the EXIF reader would read the whole file (25–60 MB) to find
        // two fields that sit near the start. Try the first 1 MiB; only if Make or the date isn't
        // there, read it all as before.
        if let Some(meta) = read_prefix(r, RAW_PREFIX).filter(|m| m.make.is_some() && m.captured_at.is_some()) {
            return meta;
        }
    }
    let _ = r.seek(SeekFrom::Start(0));
    read_meta(&mut BufReader::new(r))
}

/// How much of a TIFF-based RAW is read first for its EXIF.
pub const RAW_PREFIX: u64 = 1 << 20;

fn read_prefix(r: &mut dyn ReadSeek, len: u64) -> Option<PhotoMeta> {
    r.seek(SeekFrom::Start(0)).ok()?;
    let mut head = Vec::with_capacity(len as usize);
    r.take(len).read_to_end(&mut head).ok()?;
    Some(read_meta(&mut Cursor::new(head)))
}

/// Fuji RAF: a header, then an embedded JPEG whose EXIF holds Make and the capture time.
/// Header: magic "FUJIFILMCCD-RAW " at 0, the JPEG's offset (big-endian u32) at 84 and its length at 88.
fn read_raf(r: &mut dyn ReadSeek) -> Option<PhotoMeta> {
    r.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 92];
    r.read_exact(&mut head).ok()?;
    if &head[..16] != b"FUJIFILMCCD-RAW " {
        return None;
    }
    let offset = u32::from_be_bytes(head[84..88].try_into().ok()?) as u64;
    let len = u32::from_be_bytes(head[88..92].try_into().ok()?) as u64;
    // The EXIF block sits at the start of the JPEG; 256 KiB is plenty and bounds the read.
    let take = len.min(256 * 1024);
    r.seek(SeekFrom::Start(offset)).ok()?;
    let mut jpeg = Vec::with_capacity(take as usize);
    r.take(take).read_to_end(&mut jpeg).ok()?;
    Some(read_meta(&mut Cursor::new(jpeg)))
}

/// Folder name for a camera maker, from EXIF Make: "NIKON CORPORATION" → "Nikon", "FUJIFILM" → "Fuji".
pub fn brand_from_make(make: &str) -> Option<String> {
    let m = make.trim().to_lowercase();
    if m.is_empty() {
        return None;
    }
    const KNOWN: &[(&str, &str)] = &[
        ("nikon", "Nikon"),
        ("fuji", "Fuji"),
        ("canon", "Canon"),
        ("sony", "Sony"),
        ("apple", "Apple"),
        ("panasonic", "Panasonic"),
        ("om digital", "OM System"),
        ("olympus", "Olympus"),
        ("ricoh", "Ricoh"),
        ("pentax", "Pentax"),
        ("leica", "Leica"),
        ("hasselblad", "Hasselblad"),
        ("sigma", "Sigma"),
        ("dji", "DJI"),
        ("gopro", "GoPro"),
        ("google", "Google"),
        ("samsung", "Samsung"),
    ];
    if let Some((_, b)) = KNOWN.iter().find(|(k, _)| m.contains(k)) {
        return Some((*b).to_string());
    }
    // Unknown maker: its first word, capitalized ("ACME OPTICS" → "Acme").
    let word = m.split_whitespace().next()?;
    let mut c = word.chars();
    let first = c.next()?.to_uppercase().collect::<String>();
    Some(super::layout::sanitize(&(first + c.as_str())))
}

/// Brand from the camera's file naming, for files without EXIF (videos). Only unambiguous prefixes.
pub fn brand_from_file_name(name: &str) -> Option<String> {
    let file = name.rsplit('/').next().unwrap_or(name).to_ascii_uppercase();
    if file.starts_with("DSCF") {
        Some("Fuji".into())
    } else if file.starts_with("DSC_") || file.starts_with("_DSC") {
        Some("Nikon".into())
    } else {
        None
    }
}

/// Brand from the camera's DCIM folder name: "100_FUJI" → Fuji, "100NZ6_2"/"100NIKON" → Nikon.
pub fn brand_from_card_folder(rel_path: &str) -> Option<String> {
    let dir = rel_path.rsplit_once('/')?.0.rsplit('/').next()?.to_ascii_uppercase();
    if dir.contains("FUJI") {
        Some("Fuji".into())
    } else if dir.contains("NIKON") || (dir.len() == 8 && dir.as_bytes()[..3].iter().all(u8::is_ascii_digit) && dir.as_bytes()[3] == b'N') {
        Some("Nikon".into())
    } else {
        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brands() {
        assert_eq!(brand_from_make("NIKON CORPORATION").as_deref(), Some("Nikon"));
        assert_eq!(brand_from_make("FUJIFILM").as_deref(), Some("Fuji"));
        assert_eq!(brand_from_make("OM Digital Solutions").as_deref(), Some("OM System"));
        assert_eq!(brand_from_make("ACME OPTICS").as_deref(), Some("Acme"));
        assert_eq!(brand_from_make("  "), None);
        assert_eq!(brand_from_file_name("DCIM/100_FUJI/DSCF0001.MOV").as_deref(), Some("Fuji"));
        assert_eq!(brand_from_file_name("DCIM/100NZ6_2/DSC_0001.MOV").as_deref(), Some("Nikon"));
        assert_eq!(brand_from_file_name("IMG_0001.MOV"), None);
        assert_eq!(brand_from_card_folder("DCIM/100_FUJI/X.MOV").as_deref(), Some("Fuji"));
        assert_eq!(brand_from_card_folder("DCIM/100NZ6_2/X.MOV").as_deref(), Some("Nikon"));
        assert_eq!(brand_from_card_folder("DCIM/100CANON/X.MOV"), None);
        assert_eq!(brand_from_card_folder("X.MOV"), None);
    }

    #[test]
    fn raf_reads_the_embedded_jpeg_exif() {
        // A minimal RAF: header pointing at a JPEG that isn't valid EXIF → no metadata, no panic.
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(84, 0);
        raf.extend_from_slice(&100u32.to_be_bytes());
        raf.extend_from_slice(&4u32.to_be_bytes());
        raf.resize(100, 0);
        raf.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xD9]);
        let meta = read_file_meta(FileKind::Raw, "DSCF0001.RAF", &mut Cursor::new(raf));
        assert!(meta.make.is_none() && meta.captured_at.is_none());
        // Not a RAF at all.
        let meta = read_file_meta(FileKind::Raw, "DSCF0001.RAF", &mut Cursor::new(vec![0u8; 10]));
        assert!(meta.make.is_none());
    }
}
