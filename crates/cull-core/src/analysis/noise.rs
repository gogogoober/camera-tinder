//! Noise: how grainy is the photo?
//!
//! Rust implementation: Immerkær's fast noise estimate (one 3×3 kernel that cancels image
//! structure and keeps noise), measured on the flattest 20% of 32 px blocks of the master
//! preview so edges and texture don't count as noise. The master is already downscaled from
//! the original, so values read low compared with full-resolution tools; compare photos
//! with each other, not with other software.

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoiseLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Noise {
    /// Estimated noise standard deviation on the 0–255 scale.
    pub sigma: f32,
    pub level: NoiseLevel,
}

pub trait NoiseAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn noise(&self, img: &AnalysisImage) -> Result<Noise>;
}

const BLOCK: usize = 32;
pub const MEDIUM_SIGMA: f32 = 2.5;
pub const HIGH_SIGMA: f32 = 5.0;

pub struct RustNoise;

impl NoiseAnalyzer for RustNoise {
    fn implementation(&self) -> &'static str {
        "rust.noise.v1"
    }

    fn noise(&self, img: &AnalysisImage) -> Result<Noise> {
        let grey = img.grey()?;
        let (w, h) = (grey.width() as usize, grey.height() as usize);
        let p = grey.as_raw();
        let at = |x: usize, y: usize| p[y * w + x] as f64;

        let n = (BLOCK * BLOCK) as f64;
        // Pass 1 (cheap): every block's brightness spread. Fully clipped blocks show no noise.
        let mut blocks: Vec<(f64, usize, usize)> = Vec::new();
        let mut by = 0;
        while by + BLOCK + 2 <= h {
            let mut bx = 0;
            while bx + BLOCK + 2 <= w {
                let (mut s, mut s2) = (0.0, 0.0);
                for y in by + 1..by + BLOCK + 1 {
                    for &v in &p[y * w + bx + 1..y * w + bx + BLOCK + 1] {
                        let v = v as f64;
                        s += v;
                        s2 += v * v;
                    }
                }
                let mean = s / n;
                if mean > 4.0 && mean < 251.0 {
                    blocks.push(((s2 / n - mean * mean).max(0.0).sqrt(), bx, by));
                }
                bx += BLOCK;
            }
            by += BLOCK;
        }
        // Pass 2: Immerkær's kernel on the flattest 20% only.
        blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let flattest = &blocks[..(blocks.len() / 5).max(1).min(blocks.len())];
        let blocks: Vec<(f64, f64)> = flattest
            .iter()
            .map(|&(std, bx, by)| {
                let mut conv = 0.0;
                for y in by + 1..by + BLOCK + 1 {
                    for x in bx + 1..bx + BLOCK + 1 {
                        let k = at(x - 1, y - 1) + at(x + 1, y - 1) + at(x - 1, y + 1) + at(x + 1, y + 1)
                            - 2.0 * (at(x, y - 1) + at(x, y + 1) + at(x - 1, y) + at(x + 1, y))
                            + 4.0 * at(x, y);
                        conv += k.abs();
                    }
                }
                (std, (std::f64::consts::PI / 2.0).sqrt() * conv / (6.0 * n))
            })
            .collect();
        if blocks.is_empty() {
            return Ok(Noise { sigma: 0.0, level: NoiseLevel::Low });
        }
        let mut flat: Vec<f64> = blocks.iter().map(|b| b.1).collect();
        flat.sort_by(|a, b| a.total_cmp(b));
        let sigma = flat[flat.len() / 2] as f32;
        let level = if sigma >= HIGH_SIGMA {
            NoiseLevel::High
        } else if sigma >= MEDIUM_SIGMA {
            NoiseLevel::Medium
        } else {
            NoiseLevel::Low
        };
        Ok(Noise { sigma, level })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma};

    #[test]
    fn noisy_scores_higher_than_clean() {
        // Deterministic pseudo-random noise on a smooth gradient.
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            ((seed >> 24) as f32 / 255.0 - 0.5) * 40.0
        };
        let clean = ImageBuffer::from_fn(256, 256, |x, _| Luma([60 + (x / 4) as u8]));
        let noisy = ImageBuffer::from_fn(256, 256, |x, _| Luma([(60.0 + (x / 4) as f32 + rnd()).clamp(0.0, 255.0) as u8]));
        let c = RustNoise.noise(&AnalysisImage::from_image(DynamicImage::ImageLuma8(clean))).unwrap();
        let n = RustNoise.noise(&AnalysisImage::from_image(DynamicImage::ImageLuma8(noisy))).unwrap();
        assert!(c.sigma < 1.0, "{c:?}");
        assert_eq!(n.level, NoiseLevel::High, "{n:?}");
    }
}
