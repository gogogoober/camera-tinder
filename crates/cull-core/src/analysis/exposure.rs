//! Exposure: too dark, too bright, or fine?
//!
//! Rust implementation: a luma histogram of the working copy — mean brightness, the share of
//! crushed shadows and blown highlights, and the 5th/95th percentiles.

use serde::{Deserialize, Serialize};

use super::image::{AnalysisImage, luma};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExposureVerdict {
    Ok,
    Underexposed,
    Overexposed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exposure {
    /// Mean brightness, 0 (black) – 1 (white).
    pub mean: f32,
    /// Fraction of pixels that are (near) pure black.
    pub shadows_clipped: f32,
    /// Fraction of pixels that are (near) pure white.
    pub highlights_clipped: f32,
    /// 5th and 95th brightness percentiles, 0–1.
    pub p05: f32,
    pub p95: f32,
    pub verdict: ExposureVerdict,
}

pub trait ExposureAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn exposure(&self, img: &AnalysisImage) -> Result<Exposure>;
}

pub struct RustExposure;

impl ExposureAnalyzer for RustExposure {
    fn implementation(&self) -> &'static str {
        "rust.exposure.v1"
    }

    fn exposure(&self, img: &AnalysisImage) -> Result<Exposure> {
        let rgb = img.working_rgb()?;
        let mut hist = [0u64; 256];
        for p in rgb.pixels() {
            hist[luma(p).round().clamp(0.0, 255.0) as usize] += 1;
        }
        Ok(from_histogram(&hist))
    }
}

pub(crate) fn percentile(hist: &[u64; 256], q: f64) -> f32 {
    let n: u64 = hist.iter().sum();
    let target = (n as f64 * q).ceil() as u64;
    let mut acc = 0;
    for (i, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target.max(1) {
            return i as f32 / 255.0;
        }
    }
    1.0
}

fn from_histogram(hist: &[u64; 256]) -> Exposure {
    let n = hist.iter().sum::<u64>().max(1) as f32;
    let mean = hist.iter().enumerate().map(|(i, c)| i as f32 * *c as f32).sum::<f32>() / n / 255.0;
    let shadows_clipped = hist[..=4].iter().sum::<u64>() as f32 / n;
    let highlights_clipped = hist[251..].iter().sum::<u64>() as f32 / n;
    let (p05, p95) = (percentile(hist, 0.05), percentile(hist, 0.95));
    let verdict = if (mean < 0.18 && p95 < 0.55) || shadows_clipped > 0.35 {
        ExposureVerdict::Underexposed
    } else if (mean > 0.78 && p05 > 0.45) || highlights_clipped > 0.15 {
        ExposureVerdict::Overexposed
    } else {
        ExposureVerdict::Ok
    };
    Exposure { mean, shadows_clipped, highlights_clipped, p05, p95, verdict }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn run(f: impl Fn(u32, u32) -> u8) -> Exposure {
        let img = ImageBuffer::from_fn(200, 100, |x, y| {
            let v = f(x, y);
            Rgb([v, v, v])
        });
        RustExposure.exposure(&AnalysisImage::from_image(DynamicImage::ImageRgb8(img))).unwrap()
    }

    #[test]
    fn verdicts() {
        assert_eq!(run(|x, _| (x % 200) as u8 + 20).verdict, ExposureVerdict::Ok);
        assert_eq!(run(|x, _| (x % 40) as u8).verdict, ExposureVerdict::Underexposed);
        assert_eq!(run(|x, _| 215 + (x % 40) as u8).verdict, ExposureVerdict::Overexposed);
    }
}
