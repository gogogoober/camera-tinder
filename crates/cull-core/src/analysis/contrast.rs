//! Contrast: is the photo flat or hazy?
//!
//! Rust implementation: RMS contrast (standard deviation of brightness) and the spread
//! between the 2nd and 98th brightness percentiles.

use serde::{Deserialize, Serialize};

use super::exposure::percentile;
use super::image::{AnalysisImage, luma};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contrast {
    /// Standard deviation of brightness, 0–0.5 in practice (on a 0–1 scale).
    pub rms: f32,
    /// 98th minus 2nd brightness percentile, 0–1.
    pub range: f32,
    pub flat: bool,
}

pub trait ContrastAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn contrast(&self, img: &AnalysisImage) -> Result<Contrast>;
}

pub const FLAT_RMS: f32 = 0.08;
pub const FLAT_RANGE: f32 = 0.35;

pub struct RustContrast;

impl ContrastAnalyzer for RustContrast {
    fn implementation(&self) -> &'static str {
        "rust.contrast.v1"
    }

    fn contrast(&self, img: &AnalysisImage) -> Result<Contrast> {
        let rgb = img.working_rgb()?;
        let mut hist = [0u64; 256];
        let (mut sum, mut sum_sq, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for p in rgb.pixels() {
            let l = luma(p) as f64 / 255.0;
            hist[(l * 255.0).round().clamp(0.0, 255.0) as usize] += 1;
            sum += l;
            sum_sq += l * l;
            n += 1.0;
        }
        let n = n.max(1.0);
        let mean = sum / n;
        let rms = (sum_sq / n - mean * mean).max(0.0).sqrt() as f32;
        let range = percentile(&hist, 0.98) - percentile(&hist, 0.02);
        Ok(Contrast { rms, range, flat: rms < FLAT_RMS && range < FLAT_RANGE })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma};

    #[test]
    fn flat_vs_contrasty() {
        let run = |f: &dyn Fn(u32) -> u8| {
            let img = ImageBuffer::from_fn(200, 100, |x, _| Luma([f(x)]));
            RustContrast.contrast(&AnalysisImage::from_image(DynamicImage::ImageLuma8(img))).unwrap()
        };
        assert!(run(&|x| 120 + (x % 20) as u8).flat);
        assert!(!run(&|x| if x < 100 { 20 } else { 230 }).flat);
    }
}
