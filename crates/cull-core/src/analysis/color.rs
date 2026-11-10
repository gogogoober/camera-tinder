//! Colour cast: does the whole photo lean one colour (bad white balance)?
//!
//! Rust implementation: the gray-world assumption — averaged over a whole scene, colour should
//! come out roughly neutral. Measures the mean on two opponent axes (warm↔cool, green↔magenta)
//! relative to brightness, ignoring near-black and clipped pixels. Sunsets and neon scenes are
//! *meant* to lean, so this is a hint, never a reject on its own.

use serde::{Deserialize, Serialize};

use super::image::{AnalysisImage, luma};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CastHue {
    Neutral,
    Warm,
    Cool,
    Green,
    Magenta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorCast {
    /// Positive = warm (red/yellow), negative = cool (blue). Relative to mean brightness.
    pub warm_cool: f32,
    /// Positive = green, negative = magenta.
    pub green_magenta: f32,
    /// Overall cast strength (length of the two axes).
    pub strength: f32,
    pub hue: CastHue,
}

pub trait ColorCastAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn color_cast(&self, img: &AnalysisImage) -> Result<ColorCast>;
}

/// Strength above this is reported as a cast.
/// Calibrated on test-photos (stock, often colour-graded): ≈ the strongest 20%.
pub const CAST_STRENGTH: f32 = 0.45;

pub struct RustColorCast;

impl ColorCastAnalyzer for RustColorCast {
    fn implementation(&self) -> &'static str {
        "rust.color_cast.v2"
    }

    fn color_cast(&self, img: &AnalysisImage) -> Result<ColorCast> {
        let rgb = img.working_rgb()?;
        let (mut r, mut g, mut b, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for p in rgb.pixels() {
            let l = luma(p);
            if l < 15.0 || p.0.iter().any(|c| *c >= 250) {
                continue;
            }
            r += p[0] as f64;
            g += p[1] as f64;
            b += p[2] as f64;
            n += 1.0;
        }
        if n < 1.0 {
            return Ok(ColorCast { warm_cool: 0.0, green_magenta: 0.0, strength: 0.0, hue: CastHue::Neutral });
        }
        let (r, g, b) = (r / n, g / n, b / n);
        let mean = ((r + g + b) / 3.0).max(1.0);
        let warm_cool = ((r - b) / mean) as f32;
        let green_magenta = ((g - (r + b) / 2.0) / mean) as f32;
        let strength = (warm_cool * warm_cool + green_magenta * green_magenta).sqrt();
        let hue = if strength < CAST_STRENGTH {
            CastHue::Neutral
        } else if warm_cool.abs() >= green_magenta.abs() {
            if warm_cool > 0.0 { CastHue::Warm } else { CastHue::Cool }
        } else if green_magenta > 0.0 {
            CastHue::Green
        } else {
            CastHue::Magenta
        };
        Ok(ColorCast { warm_cool, green_magenta, strength, hue })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn run(c: [u8; 3]) -> ColorCast {
        let img = ImageBuffer::from_fn(64, 64, |x, _| Rgb([c[0].saturating_add((x % 8) as u8), c[1], c[2]]));
        RustColorCast.color_cast(&AnalysisImage::from_image(DynamicImage::ImageRgb8(img))).unwrap()
    }

    #[test]
    fn detects_casts() {
        assert_eq!(run([120, 120, 120]).hue, CastHue::Neutral);
        assert_eq!(run([200, 140, 80]).hue, CastHue::Warm);
        assert_eq!(run([80, 120, 200]).hue, CastHue::Cool);
        assert_eq!(run([90, 170, 90]).hue, CastHue::Green);
    }
}
