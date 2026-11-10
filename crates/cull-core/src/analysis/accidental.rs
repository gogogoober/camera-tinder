//! Accidental shots: pocket shots, lens cap on, pointed at the ground or a blank wall.
//!
//! Rust implementation: nearly black, or nearly no detail *and* no contrast at all. Kept
//! deliberately conservative — it should only catch frames nobody meant to take.

use serde::{Deserialize, Serialize};

use super::image::{AnalysisImage, luma};
use crate::error::Result;
use super::blur::{laplacian_variance, squash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccidentalReason {
    /// Almost black (lens cap, pocket).
    Dark,
    /// Almost uniform (blank wall, sky, ground, motion smear).
    Blank,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accidental {
    pub likely: bool,
    pub reason: Option<AccidentalReason>,
}

pub trait AccidentalAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn accidental(&self, img: &AnalysisImage) -> Result<Accidental>;
}

pub struct RustAccidental;

impl AccidentalAnalyzer for RustAccidental {
    fn implementation(&self) -> &'static str {
        "rust.accidental.v1"
    }

    fn accidental(&self, img: &AnalysisImage) -> Result<Accidental> {
        let rgb = img.working_rgb()?;
        let (mut sum, mut sum_sq, mut n, mut bright) = (0.0f64, 0.0f64, 0.0f64, 0u64);
        for p in rgb.pixels() {
            let l = luma(p) as f64;
            sum += l;
            sum_sq += l * l;
            n += 1.0;
            if l > 60.0 {
                bright += 1;
            }
        }
        let n = n.max(1.0);
        let mean = sum / n;
        let std = (sum_sq / n - mean * mean).max(0.0).sqrt();
        let bright_frac = bright as f64 / n;

        let reason = if mean < 12.0 && bright_frac < 0.01 {
            Some(AccidentalReason::Dark)
        } else {
            let grey = img.working_grey()?;
            let detail = squash(laplacian_variance(grey, 0, 0, grey.width(), grey.height()));
            (detail < 4 && std < 10.0).then_some(AccidentalReason::Blank)
        };
        Ok(Accidental { likely: reason.is_some(), reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma};

    fn run(f: impl Fn(u32, u32) -> u8) -> Accidental {
        let img = ImageBuffer::from_fn(200, 100, |x, y| Luma([f(x, y)]));
        RustAccidental.accidental(&AnalysisImage::from_image(DynamicImage::ImageLuma8(img))).unwrap()
    }

    #[test]
    fn catches_dark_and_blank_only() {
        assert_eq!(run(|x, _| (x % 5) as u8).reason, Some(AccidentalReason::Dark));
        assert_eq!(run(|_, _| 140).reason, Some(AccidentalReason::Blank));
        assert!(!run(|x, y| if (x / 5 + y / 5) % 2 == 0 { 40 } else { 200 }).likely);
    }
}
