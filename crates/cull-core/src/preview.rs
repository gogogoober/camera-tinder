//! Display-sized previews, generated once and cached on disk.
//!
//! Full-size camera JPEGs (24–45 MP) are too slow to push to a UI directly.
//! We decode once, apply EXIF orientation, downscale, and cache a small JPEG
//! keyed by content fingerprint + size. Concurrent decodes are capped to keep
//! memory bounded (this matters a lot on iOS).

use std::fs;
use std::io::{BufRead, BufReader, Seek};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::error::{CoreError, Result};
use crate::model::{MediaKind, Photo};
use crate::ports::MediaSource;

pub const MIN_EDGE: u32 = 64;
pub const MAX_EDGE: u32 = 4096;

pub struct PreviewCache {
    dir: PathBuf,
    slots: Mutex<usize>,
    freed: Condvar,
    tmp_counter: AtomicU64,
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
        Ok(Self { dir, slots: Mutex::new(slots), freed: Condvar::new(), tmp_counter: AtomicU64::new(0) })
    }

    fn acquire(&self) -> Permit<'_> {
        let mut slots = self.slots.lock().unwrap();
        while *slots == 0 {
            slots = self.freed.wait(slots).unwrap();
        }
        *slots -= 1;
        Permit(self)
    }

    /// JPEG bytes of `photo` with its longest edge at most `max_edge`.
    pub fn get(&self, photo: &Photo, source: &dyn MediaSource, max_edge: u32) -> Result<Vec<u8>> {
        let max_edge = max_edge.clamp(MIN_EDGE, MAX_EDGE);
        let path = self.dir.join(format!("{}_{}.jpg", photo.fingerprint, max_edge));
        if let Ok(bytes) = fs::read(&path) {
            return Ok(bytes);
        }
        if photo.kind != MediaKind::Jpeg {
            return Err(CoreError::Unsupported(format!("preview for {:?}", photo.kind)));
        }

        let _permit = self.acquire();
        // Another request may have produced it while we waited.
        if let Ok(bytes) = fs::read(&path) {
            return Ok(bytes);
        }
        let reader = BufReader::new(source.open(&photo.rel_path)?);
        let bytes = render_jpeg(reader, max_edge)?;

        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(".{}_{}.{}.{n}.tmp", photo.fingerprint, max_edge, std::process::id()));
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(bytes)
    }
}

/// Decode, auto-rotate per EXIF, downscale, re-encode as JPEG.
pub fn render_jpeg<R: BufRead + Seek>(reader: R, max_edge: u32) -> Result<Vec<u8>> {
    let mut decoder = ImageReader::new(reader).with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    if img.width() > max_edge || img.height() > max_edge {
        img = img.thumbnail(max_edge, max_edge);
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8())?;
    Ok(out)
}
