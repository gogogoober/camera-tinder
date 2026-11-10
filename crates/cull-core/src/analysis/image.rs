//! `AnalysisImage`: the one input every analyzer receives.
//!
//! It wraps the photo's cached **master preview** (≤ 2048 px, already EXIF-rotated, see
//! `preview.rs`), so analysis never touches the original and works with the card unplugged.
//! Decoded forms are produced lazily and shared, so ten analyzers cost one decode.
//!
//! Every coordinate analyzers report is relative to this (upright) image.

use std::sync::OnceLock;

use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, RgbImage};

use crate::error::{CoreError, Result};
use crate::preview;

/// Long edge of the small working copy used by the cheap whole-image measures.
pub const WORKING_EDGE: u32 = 640;

type Lazy<T> = OnceLock<std::result::Result<T, String>>;

pub struct AnalysisImage {
    jpeg: Lazy<Vec<u8>>,
    decoded: Lazy<DynamicImage>,
    rgb: Lazy<RgbImage>,
    grey: Lazy<GrayImage>,
    working_rgb: Lazy<RgbImage>,
    working_grey: Lazy<GrayImage>,
}

fn get<T>(cell: &Lazy<T>, make: impl FnOnce() -> Result<T>) -> Result<&T> {
    cell.get_or_init(|| make().map_err(|e| e.to_string())).as_ref().map_err(|e| CoreError::Other(e.clone()))
}

impl AnalysisImage {
    fn empty() -> Self {
        Self {
            jpeg: OnceLock::new(),
            decoded: OnceLock::new(),
            rgb: OnceLock::new(),
            grey: OnceLock::new(),
            working_rgb: OnceLock::new(),
            working_grey: OnceLock::new(),
        }
    }

    /// From encoded JPEG bytes (normally the cached master preview). EXIF orientation,
    /// if any, is applied on decode.
    pub fn from_jpeg(bytes: Vec<u8>) -> Self {
        let s = Self::empty();
        let _ = s.jpeg.set(Ok(bytes));
        s
    }

    /// From an already decoded, upright image. Shrunk to the master size so results match
    /// what the app computes from its preview cache.
    pub fn from_image(img: DynamicImage) -> Self {
        let s = Self::empty();
        let _ = s.decoded.set(Ok(preview::shrink(&img, preview::MASTER_EDGE)));
        s
    }

    /// Encoded JPEG of the upright image (for platform engines such as Apple Vision).
    pub fn jpeg(&self) -> Result<&[u8]> {
        get(&self.jpeg, || preview::encode(self.decoded()?)).map(|v| v.as_slice())
    }

    pub fn decoded(&self) -> Result<&DynamicImage> {
        get(&self.decoded, || preview::decode(self.jpeg()?))
    }

    /// Full-resolution (≤ 2048 px) RGB.
    pub fn rgb(&self) -> Result<&RgbImage> {
        get(&self.rgb, || Ok(self.decoded()?.to_rgb8()))
    }

    /// Full-resolution (≤ 2048 px) luma. Used for region measures (faces, subjects) and noise.
    pub fn grey(&self) -> Result<&GrayImage> {
        get(&self.grey, || Ok(self.decoded()?.to_luma8()))
    }

    /// Small (≤ 640 px) RGB copy for whole-image colour/tone measures.
    pub fn working_rgb(&self) -> Result<&RgbImage> {
        get(&self.working_rgb, || {
            let img = self.decoded()?;
            Ok(if img.width() > WORKING_EDGE || img.height() > WORKING_EDGE {
                img.resize(WORKING_EDGE, WORKING_EDGE, FilterType::Triangle).to_rgb8()
            } else {
                img.to_rgb8()
            })
        })
    }

    /// Small (≤ 640 px) luma copy, so sharpness scores compare across 12 MP and 45 MP cameras.
    pub fn working_grey(&self) -> Result<&GrayImage> {
        get(&self.working_grey, || Ok(DynamicImage::ImageRgb8(self.working_rgb()?.clone()).to_luma8()))
    }

    /// `(width, height)` of the full-resolution image.
    pub fn dimensions(&self) -> Result<(u32, u32)> {
        let d = self.decoded()?;
        Ok((d.width(), d.height()))
    }
}

/// Rec. 709 luma of an RGB pixel, 0–255.
pub(crate) fn luma(p: &image::Rgb<u8>) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}
