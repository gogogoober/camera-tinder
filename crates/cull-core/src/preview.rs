//! Display-sized previews, generated once and cached on disk.
//!
//! Full-size camera JPEGs (24–45 MP) are too slow to push to a UI directly.
//! At scan time the original is decoded once (by the platform's `PreviewMaker`) into a
//! 2048 px master and a 256 px thumbnail, cached by content fingerprint + size. Other sizes
//! are made from the master on demand. Concurrent on-demand renders are capped to keep
//! memory bounded (this matters a lot on iOS).

use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader};


use crate::error::{CoreError, Result};
use crate::identity::fingerprint;
use crate::model::{MediaKind, Photo};
use crate::ports::{MediaSource, PreviewMaker};

pub const MIN_EDGE: u32 = 64;
pub const MAX_EDGE: u32 = 4096;
/// The one full-quality preview made from the original at scan time. Every other
/// size is derived from it, so culling never needs the card.
pub const MASTER_EDGE: u32 = 2048;
/// Made alongside the master: filmstrip / grid thumbnails.
pub const THUMB_EDGE: u32 = 256;

pub struct PreviewCache {
    dir: PathBuf,
    slots: Mutex<usize>,
    freed: Condvar,
    tmp_counter: AtomicU64,
    maker: RwLock<Arc<dyn PreviewMaker>>,
}

struct Permit<'a>(&'a PreviewCache);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.slots.lock().unwrap() += 1;
        self.0.freed.notify_one();
    }
}

impl PreviewCache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let slots = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
        Ok(Self { dir, slots: Mutex::new(slots), freed: Condvar::new(), tmp_counter: AtomicU64::new(0), maker: RwLock::new(Arc::new(RustPreviewMaker)) })
    }

    fn acquire(&self) -> Permit<'_> {
        let mut slots = self.slots.lock().unwrap();
        while *slots == 0 {
            slots = self.freed.wait(slots).unwrap();
        }
        *slots -= 1;
        Permit(self)
    }

    fn path(&self, fingerprint: &str, edge: u32) -> PathBuf {
        self.dir.join(format!("{fingerprint}_{edge}.jpg"))
    }

    /// Write into the cache dir only: a new temp file (never an existing one), then rename into place.
    fn write_cached(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(".{}.{}.{n}.tmp", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
        let written = OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| f.write_all(bytes));
        if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
            let _ = fs::remove_file(&tmp); // our own temp file only
            return Err(e.into());
        }
        Ok(())
    }

    /// True once this photo has a complete master preview, i.e. it never needs the original again
    /// to be viewed. A damaged one (e.g. cut short by a power loss) doesn't count, so it is rebuilt.
    pub fn has_master(&self, fingerprint: &str) -> bool {
        let Ok(mut f) = File::open(self.path(fingerprint, MASTER_EDGE)) else { return false };
        let (mut head, mut tail) = ([0u8; 2], [0u8; 2]);
        f.read_exact(&mut head).is_ok() && f.seek(SeekFrom::End(-2)).is_ok() && f.read_exact(&mut tail).is_ok() && is_complete_jpeg_ends(head, tail)
    }

    /// A cached preview, if it's there and complete.
    fn read_cached(&self, path: &Path) -> Option<Vec<u8>> {
        fs::read(path).ok().filter(|b| b.len() >= 4 && is_complete_jpeg_ends([b[0], b[1]], [b[b.len() - 2], b[b.len() - 1]]))
    }

    /// The maker's output, with a crash inside a decoder turned into an error for this one photo.
    fn render(&self, jpeg: &[u8], edges: &[u32]) -> Result<Vec<Vec<u8>>> {
        let maker = self.maker();
        catch_unwind(AssertUnwindSafe(|| maker.render(jpeg, edges))).unwrap_or_else(|_| Err(CoreError::Other("the image decoder crashed on this file".into())))
    }

    /// Use a platform's (faster) preview maker instead of the pure-Rust one.
    pub fn set_maker(&self, maker: Arc<dyn PreviewMaker>) {
        *self.maker.write().unwrap() = maker;
    }

    pub fn maker_name(&self) -> &'static str {
        self.maker.read().unwrap().implementation()
    }

    fn maker(&self) -> Arc<dyn PreviewMaker> {
        self.maker.read().unwrap().clone()
    }

    /// Scan time: store the master and thumbnail made from the original's bytes (one decode).
    /// Returns false, storing nothing, when the original can't be decoded.
    pub fn store_from_original(&self, fingerprint: &str, original: &[u8]) -> Result<bool> {
        let Ok(rendered) = self.render(original, &[MASTER_EDGE, THUMB_EDGE]) else { return Ok(false) };
        let [master, thumb] = <[Vec<u8>; 2]>::try_from(rendered).map_err(|_| CoreError::Other("preview maker returned the wrong number of sizes".into()))?;
        self.write_cached(&self.path(fingerprint, MASTER_EDGE), &master)?;
        self.write_cached(&self.path(fingerprint, THUMB_EDGE), &thumb)?;
        Ok(true)
    }

    /// JPEG bytes of `photo` with its longest edge at most `max_edge` (capped at the master size).
    ///
    /// Served from the cache; sizes other than master/thumb are derived from the master.
    /// The original is only read for photos indexed before masters existed (a rescan fixes those).
    pub fn get(&self, photo: &Photo, source: Option<&dyn MediaSource>, max_edge: u32) -> Result<Vec<u8>> {
        let edge = max_edge.clamp(MIN_EDGE, MAX_EDGE).min(MASTER_EDGE);
        let path = self.path(&photo.fingerprint, edge);
        if let Some(bytes) = self.read_cached(&path) {
            return Ok(bytes);
        }
        if photo.kind != MediaKind::Jpeg {
            return Err(CoreError::Unsupported(format!("preview for {:?}", photo.kind)));
        }

        let _permit = self.acquire();
        // Another request may have produced it while we waited.
        if let Some(bytes) = self.read_cached(&path) {
            return Ok(bytes);
        }
        if !self.has_master(&photo.fingerprint) {
            let Some(source) = source else {
                return Err(CoreError::Other(format!("no preview cached for {} and its source is not connected", photo.file_name)));
            };
            if photo.size > crate::scan::MAX_PHOTO_BYTES {
                return Err(CoreError::Other(format!("{} is too large to preview", photo.file_name)));
            }
            let mut bytes = Vec::with_capacity(photo.size as usize);
            source.open(&photo.rel_path)?.take(photo.size + 1).read_to_end(&mut bytes)?;
            // Only from the very same photo: after a card swap another photo can sit at this path.
            if bytes.len() as u64 != photo.size || fingerprint(&mut Cursor::new(&bytes[..]), photo.size)? != photo.fingerprint {
                return Err(CoreError::Other(format!("{} has changed since it was scanned; rescan the folder", photo.file_name)));
            }
            if !self.store_from_original(&photo.fingerprint, &bytes)? {
                return Err(CoreError::Other(format!("can't decode {}", photo.file_name)));
            }
        }
        let master = self.read_cached(&self.path(&photo.fingerprint, MASTER_EDGE)).ok_or_else(|| CoreError::Other(format!("no preview for {}", photo.file_name)))?;
        if edge == MASTER_EDGE {
            return Ok(master);
        }
        let bytes = self.render(&master, &[edge])?.pop().ok_or_else(|| CoreError::Other("preview maker returned nothing".into()))?;
        self.write_cached(&path, &bytes)?;
        Ok(bytes)
    }
}

/// JPEG start (FFD8) and end (FFD9) markers: a preview that was cut short has no end marker.
fn is_complete_jpeg_ends(head: [u8; 2], tail: [u8; 2]) -> bool {
    head == [0xFF, 0xD8] && tail == [0xFF, 0xD9]
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `<32 lowercase hex>_<edge>.jpg`: exactly the names `PreviewCache::path` produces.
fn is_preview_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jpg") else { return false };
    let Some((fp, edge)) = stem.split_once('_') else { return false };
    fp.len() == 32 && fp.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) && is_digits(edge)
}

/// Only names this cache creates: previews, and `.<preview name>.<pid>.<n>.tmp` leftovers
/// from `write_cached` (e.g. after a crash). Anything else is not ours and is never deleted.
fn is_cache_file(name: &str) -> bool {
    if is_preview_name(name) {
        return true;
    }
    let Some(inner) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) else { return false };
    let mut parts = inner.rsplitn(3, '.');
    let (Some(n), Some(pid), Some(preview)) = (parts.next(), parts.next(), parts.next()) else { return false };
    is_digits(n) && is_digits(pid) && is_preview_name(preview)
}

impl PreviewCache {
    /// (file count, bytes) of cached previews.
    pub fn usage(&self) -> Result<(u64, u64)> {
        let (mut files, mut bytes) = (0, 0);
        for e in fs::read_dir(&self.dir)? {
            let e = e?;
            let m = e.metadata()?;
            if m.is_file() && is_cache_file(&e.file_name().to_string_lossy()) {
                files += 1;
                bytes += m.len();
            }
        }
        Ok((files, bytes))
    }

    /// Delete every cached preview. Touches nothing but this cache's own files:
    /// - only directly inside the cache folder (never recursive; never follows symlinks),
    /// - only regular files (a symlink, folder or anything else is skipped),
    /// - only names the cache itself creates (`is_cache_file`).
    ///
    /// Removing a name never destroys data reachable through another link, so even a
    /// hard link someone placed here can't take an original with it.
    pub fn clear(&self) -> Result<u64> {
        // The cache folder itself must be a real folder, not a symlink to somewhere else.
        if !fs::symlink_metadata(&self.dir)?.is_dir() {
            return Err(CoreError::Other(format!("preview cache {} is not a plain folder; not clearing", self.dir.display())));
        }
        let mut freed = 0;
        for e in fs::read_dir(&self.dir)? {
            let e = e?;
            let m = fs::symlink_metadata(e.path())?;
            if m.is_file() && is_cache_file(&e.file_name().to_string_lossy()) {
                let path = e.path();
                fs::remove_file(&path)?;
                freed += m.len();
            }
        }
        Ok(freed)
    }
}

/// The portable preview maker: full decode with the `image` crate, then downscale and encode.
/// Slower than a native codec (it can't decode at a reduced size), but runs everywhere.
pub struct RustPreviewMaker;

impl PreviewMaker for RustPreviewMaker {
    fn implementation(&self) -> &'static str {
        "rust.image.v1"
    }

    fn render(&self, jpeg: &[u8], edges: &[u32]) -> Result<Vec<Vec<u8>>> {
        let mut img = decode(jpeg)?;
        let mut out = Vec::with_capacity(edges.len());
        for &edge in edges {
            img = shrink(&img, edge);
            out.push(encode(&img)?);
        }
        Ok(out)
    }
}

/// Decode a JPEG held in memory and apply its EXIF orientation.
pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    let mut decoder = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

pub(crate) fn shrink(img: &DynamicImage, edge: u32) -> DynamicImage {
    if img.width() > edge || img.height() > edge { img.thumbnail(edge, edge) } else { img.clone() }
}

pub(crate) fn encode(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::is_cache_file;

    #[test]
    fn only_our_own_files_count_as_cache() {
        assert!(is_cache_file("0123456789abcdef0123456789abcdef_2048.jpg"));
        assert!(is_cache_file(".0123456789abcdef0123456789abcdef_2048.jpg.123.4.tmp"));
        for name in [
            "DSC_0001.JPG",
            "photo.jpg",
            "0123_2048.jpg",
            "0123456789abcdef0123456789abcdef_2048.JPG",
            "0123456789ABCDEF0123456789ABCDEF_2048.jpg",
            "0123456789abcdef0123456789abcdef_.jpg",
            "0123456789abcdef0123456789abcdef_2048.jpg.bak",
            "camera-tinder.db",
            ".DS_Store",
            ".notes.tmp",
            ".DSC_0001.JPG.123.4.tmp",
            ".0123456789abcdef0123456789abcdef_2048.jpg.x.4.tmp",
        ] {
            assert!(!is_cache_file(name), "{name}");
        }
    }
}
