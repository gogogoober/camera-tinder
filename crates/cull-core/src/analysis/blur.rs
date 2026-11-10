//! Blur: is the photo soft, and if so is it camera/subject motion or missed focus?
//!
//! Rust implementation: Laplacian variance (as at scan time) over the whole frame **and**
//! per tile. `peak` averages the sharpest 10% of tiles, so a sharp subject against a creamy
//! background is not called soft. When the frame is soft, the gradient structure tensor tells
//! motion blur (edges survive in one direction) from defocus (soft in every direction).

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;
use crate::sharpness::{laplacian_variance, squash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlurVerdict {
    Sharp,
    /// Soft everywhere: missed focus, or a genuinely featureless scene.
    Soft,
    /// Soft, with detail surviving in one direction: shake or a moving subject.
    MotionBlur,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blur {
    /// Whole-frame sharpness 0–100 (same scale as `Photo::sharpness`).
    pub score: u8,
    /// Sharpness of the sharpest 10% of the frame, 0–100.
    pub peak: u8,
    /// 0 = edges in every direction, 1 = all edges aligned (typical of motion blur).
    pub directionality: f32,
    pub verdict: BlurVerdict,
}

pub trait BlurAnalyzer: Send + Sync {
    /// Stable id of this implementation; stored with results so a change re-runs them.
    fn implementation(&self) -> &'static str;
    fn blur(&self, img: &AnalysisImage) -> Result<Blur>;
}

/// Tiles per side for the peak measure.
const GRID: u32 = 8;
/// Peak sharpness below this means nothing in the frame is in focus.
pub const SOFT_PEAK: u8 = 35;
/// Directionality above this, on a soft frame, reads as motion blur.
pub const MOTION_DIRECTIONALITY: f32 = 0.45;

pub struct RustBlur;

impl BlurAnalyzer for RustBlur {
    fn implementation(&self) -> &'static str {
        "rust.blur.v1"
    }

    fn blur(&self, img: &AnalysisImage) -> Result<Blur> {
        let grey = img.working_grey()?;
        let (w, h) = grey.dimensions();
        if w < 3 || h < 3 {
            return Ok(Blur { score: 0, peak: 0, directionality: 0.0, verdict: BlurVerdict::Soft });
        }
        let score = squash(laplacian_variance(grey, 0, 0, w, h));

        let (tw, th) = ((w / GRID).max(3), (h / GRID).max(3));
        let mut tiles: Vec<f64> = Vec::new();
        for ty in 0..GRID {
            for tx in 0..GRID {
                if tx * tw + 2 < w && ty * th + 2 < h {
                    tiles.push(laplacian_variance(grey, tx * tw, ty * th, tw, th));
                }
            }
        }
        tiles.sort_by(|a, b| b.total_cmp(a));
        let top = (tiles.len() / 10).max(1);
        let peak = squash(tiles[..top].iter().sum::<f64>() / top as f64);

        let directionality = directionality(grey);
        let verdict = if peak >= SOFT_PEAK {
            BlurVerdict::Sharp
        } else if directionality >= MOTION_DIRECTIONALITY {
            BlurVerdict::MotionBlur
        } else {
            BlurVerdict::Soft
        };
        Ok(Blur { score, peak, directionality, verdict })
    }
}

/// Coherence of the gradient structure tensor over the whole image.
fn directionality(grey: &image::GrayImage) -> f32 {
    let (w, h) = (grey.width() as usize, grey.height() as usize);
    let p = grey.as_raw();
    let at = |x: usize, y: usize| p[y * w + x] as f64;
    let (mut jxx, mut jyy, mut jxy) = (0.0f64, 0.0f64, 0.0f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1);
            let gy = at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1);
            jxx += gx * gx;
            jyy += gy * gy;
            jxy += gx * gy;
        }
    }
    let trace = jxx + jyy;
    if trace <= f64::EPSILON {
        return 0.0;
    }
    (((jxx - jyy).powi(2) + 4.0 * jxy * jxy).sqrt() / trace) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma};

    fn img(f: impl Fn(u32, u32) -> u8) -> AnalysisImage {
        AnalysisImage::from_image(DynamicImage::ImageLuma8(ImageBuffer::from_fn(480, 320, |x, y| Luma([f(x, y)]))))
    }

    #[test]
    fn sharp_soft_and_motion() {
        let checker = |x: u32, y: u32| if (x / 6 + y / 6) % 2 == 0 { 30 } else { 220 };
        let sharp = RustBlur.blur(&img(checker)).unwrap();
        assert_eq!(sharp.verdict, BlurVerdict::Sharp, "{sharp:?}");

        let soft_src = DynamicImage::ImageLuma8(ImageBuffer::from_fn(480, 320, |x, y| Luma([checker(x, y)]))).blur(8.0);
        let soft = RustBlur.blur(&AnalysisImage::from_image(soft_src)).unwrap();
        assert_ne!(soft.verdict, BlurVerdict::Sharp, "{soft:?}");

        // Gentle horizontal stripes only: all edges point one way.
        let streaks = RustBlur.blur(&img(|_, y| (128.0 + 20.0 * ((y as f32) / 9.0).sin()) as u8)).unwrap();
        assert!(streaks.directionality > 0.9, "{streaks:?}");
    }

    #[test]
    fn sharp_subject_on_soft_background_is_sharp() {
        // Flat frame with one small detailed patch.
        let f = |x: u32, y: u32| if (200..260).contains(&x) && (120..180).contains(&y) && (x / 3 + y / 3) % 2 == 0 { 250 } else { 120 };
        let b = RustBlur.blur(&img(f)).unwrap();
        assert!(b.peak > b.score, "{b:?}");
        assert_eq!(b.verdict, BlurVerdict::Sharp);
    }
}
