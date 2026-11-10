//! Blur: is the photo soft, and if so is it camera/subject motion or missed focus?
//!
//! Rust implementation: Laplacian variance over the whole frame **and**
//! per tile. `peak` averages the sharpest 10% of tiles, so a sharp subject against a creamy
//! background is not called soft. When the frame is soft, the gradient structure tensor tells
//! motion blur (edges survive in one direction) from defocus (soft in every direction).

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;
use image::GrayImage;

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
    /// Whole-frame sharpness 0–100.
    pub score: u8,
    /// Sharpness of the sharpest 10% of the frame, 0–100.
    pub peak: u8,
    /// 0 = edges in every direction, 1 = all edges aligned (typical of motion blur).
    pub directionality: f32,
    pub verdict: BlurVerdict,
    /// Share of strong edges that change over 4 px or less (at 1024 px), 0–1. A frame swung by hand has busy
    /// texture that fools the tile measure above, but its edges are smeared: this is low (< `SMEARED_EDGE_FRACTION`).
    /// `None` when the image is too small, or has too few edges, to judge.
    #[serde(default)]
    pub edge_sharpness: Option<f32>,
}

/// Variance at which `squash` reads 50.
const HALF_SCORE_VARIANCE: f64 = 150.0;

/// Variance of the 4-neighbour Laplacian inside `x..x+w`, `y..y+h` of `grey`. Sharp images
/// have strong edges (high variance); blurry ones are smooth (low variance).
/// Shared with `accidental` and `subject_focus`.
pub fn laplacian_variance(grey: &GrayImage, x: u32, y: u32, w: u32, h: u32) -> f64 {
    let (iw, ih) = (grey.width() as usize, grey.height() as usize);
    let (x0, y0) = ((x as usize).max(1), (y as usize).max(1));
    let (x1, y1) = (((x + w) as usize).min(iw - 1), ((y + h) as usize).min(ih - 1));
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let px = grey.as_raw();
    let (mut sum, mut sum_sq, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let c = px[yy * iw + xx] as f64;
            let lap = px[(yy - 1) * iw + xx] as f64 + px[(yy + 1) * iw + xx] as f64 + px[yy * iw + xx - 1] as f64 + px[yy * iw + xx + 1] as f64 - 4.0 * c;
            sum += lap;
            sum_sq += lap * lap;
            n += 1.0;
        }
    }
    let mean = sum / n;
    (sum_sq / n - mean * mean).max(0.0)
}

/// Map a Laplacian variance to the 0–100 sharpness scale.
pub fn squash(variance: f64) -> u8 {
    (100.0 * variance / (variance + HALF_SCORE_VARIANCE)).round().clamp(0.0, 100.0) as u8
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
/// Below this share of crisp edges a frame counts as smeared even when some tiles look sharp (a swung camera over a
/// textured scene). Calibrated on the test set: swung / shaken shots 0.11–0.18; the softest ordinary photo 0.28.
pub const SMEARED_EDGE_FRACTION: f32 = 0.22;
/// Fewer strong edges than this: not enough to judge.
const MIN_EDGES: usize = 800;
/// Smaller images than this are not judged (their edges are naturally wide after enlarging).
const EDGE_MIN_LONG_SIDE: u32 = 900;
const EDGE_LONG_SIDE: u32 = 1024;
/// Directionality above this, on a soft frame, reads as motion blur.
pub const MOTION_DIRECTIONALITY: f32 = 0.45;

pub struct RustBlur;

impl BlurAnalyzer for RustBlur {
    fn implementation(&self) -> &'static str {
        "rust.blur.v2"
    }

    fn blur(&self, img: &AnalysisImage) -> Result<Blur> {
        let grey = img.working_grey()?;
        let (w, h) = grey.dimensions();
        if w < 3 || h < 3 {
            return Ok(Blur { score: 0, peak: 0, directionality: 0.0, verdict: BlurVerdict::Soft, edge_sharpness: None });
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
        let edge_sharpness = edge_sharpness(img.grey()?);
        let verdict = if peak >= SOFT_PEAK {
            if edge_sharpness.is_some_and(|f| f < SMEARED_EDGE_FRACTION) { BlurVerdict::MotionBlur } else { BlurVerdict::Sharp }
        } else if directionality >= MOTION_DIRECTIONALITY {
            BlurVerdict::MotionBlur
        } else {
            BlurVerdict::Soft
        };
        Ok(Blur { score, peak, directionality, verdict, edge_sharpness })
    }
}

/// Widths (px) of the strong edges along every second row (`vertical = false`) or column.
fn edge_widths(g: &[u8], w: usize, h: usize, vertical: bool, out: &mut Vec<u32>) {
    let (lines, len) = if vertical { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| -> i32 { if vertical { g[i * w + line] as i32 } else { g[line * w + i] as i32 } };
    let mut d = vec![0i32; len];
    for line in (0..lines).step_by(2) {
        for i in 1..len - 1 {
            d[i] = at(line, i + 1) - at(line, i - 1);
        }
        for i in 2..len - 2 {
            let a = d[i].abs();
            if a < 16 || a < d[i - 1].abs() || a <= d[i + 1].abs() {
                continue;
            }
            let s = d[i].signum();
            let thr = (a as f32 * 0.25) as i32;
            let (mut l, mut r) = (i, i);
            while l > 1 && s * d[l - 1] > thr {
                l -= 1;
            }
            while r + 2 < len && s * d[r + 1] > thr {
                r += 1;
            }
            if (at(line, r + 1) - at(line, l - 1)).abs() >= 40 {
                out.push((r - l + 1) as u32);
            }
        }
    }
}

/// Share of strong edges that are crisp (change over ≤ 4 px at 1024 px). See [`Blur::edge_sharpness`].
fn edge_sharpness(grey: &GrayImage) -> Option<f32> {
    let long = grey.width().max(grey.height());
    if long < EDGE_MIN_LONG_SIDE {
        return None;
    }
    let small;
    let g = if long > EDGE_LONG_SIDE {
        let k = EDGE_LONG_SIDE as f32 / long as f32;
        small = image::imageops::resize(grey, (grey.width() as f32 * k).round() as u32, (grey.height() as f32 * k).round() as u32, image::imageops::FilterType::Triangle);
        &small
    } else {
        grey
    };
    let (w, h) = (g.width() as usize, g.height() as usize);
    let mut widths = Vec::new();
    edge_widths(g.as_raw(), w, h, false, &mut widths);
    edge_widths(g.as_raw(), w, h, true, &mut widths);
    (widths.len() >= MIN_EDGES).then(|| widths.iter().filter(|&&x| x <= 4).count() as f32 / widths.len() as f32)
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

    #[test]
    fn crisp_edges_score_high_smeared_edges_low_and_small_images_are_not_judged() {
        let crisp = ImageBuffer::from_fn(1200, 800, |x, _| Luma([if (x / 20) % 2 == 0 { 30u8 } else { 220 }]));
        assert!(edge_sharpness(&crisp).unwrap() > 0.9);
        // The same stripes with 14 px ramps instead of steps.
        let smeared = ImageBuffer::from_fn(1200, 800, |x, _| {
            let p = (x % 40) as f32;
            let v = if p < 20.0 { (p / 14.0).min(1.0) } else { 1.0 - ((p - 20.0) / 14.0).min(1.0) };
            Luma([(30.0 + 190.0 * v) as u8])
        });
        assert!(edge_sharpness(&smeared).unwrap() < SMEARED_EDGE_FRACTION);
        let small = ImageBuffer::from_fn(400, 300, |x, _| Luma([if (x / 20) % 2 == 0 { 30u8 } else { 220 }]));
        assert_eq!(edge_sharpness(&small), None);
    }
}
