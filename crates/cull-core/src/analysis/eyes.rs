//! Eyes: are anyone's eyes closed?
//!
//! Input: the faces from `faces` (any engine). Rust implementation: the eye aspect ratio —
//! height of the eye outline divided by its width, in pixels. Open eyes are roughly
//! 0.25–0.45; closed eyes collapse towards 0.1.

use serde::{Deserialize, Serialize};

use super::faces::Face;
use super::geometry::NormPoint;
use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EyeState {
    /// Index into the photo's face list.
    pub face: usize,
    /// Eye aspect ratio (height / width), `None` when that eye's outline is missing.
    pub left_openness: Option<f32>,
    pub right_openness: Option<f32>,
    /// Both measured eyes are below the closed threshold.
    pub closed: bool,
}

pub trait EyeStateAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn eyes(&self, img: &AnalysisImage, faces: &[Face]) -> Result<Vec<EyeState>>;
}

/// Openness below this reads as closed.
pub const CLOSED_OPENNESS: f32 = 0.16;
/// Faces smaller than this (fraction of image width) are too small to judge eyes.
pub const MIN_FACE_WIDTH: f32 = 0.04;

pub struct AspectRatioEyes;

/// Eye aspect ratio from an outline, in pixel space.
pub fn openness(points: &[NormPoint], width: u32, height: u32) -> Option<f32> {
    if points.len() < 4 {
        return None;
    }
    let xs = points.iter().map(|p| p.x * width as f32);
    let ys = points.iter().map(|p| p.y * height as f32);
    let (x0, x1) = xs.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let (y0, y1) = ys.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let w = x1 - x0;
    (w > 0.5).then(|| (y1 - y0) / w)
}

impl EyeStateAnalyzer for AspectRatioEyes {
    fn implementation(&self) -> &'static str {
        "rust.eye_aspect_ratio.v1"
    }

    fn eyes(&self, img: &AnalysisImage, faces: &[Face]) -> Result<Vec<EyeState>> {
        let (w, h) = img.dimensions()?;
        Ok(faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.bounds.width >= MIN_FACE_WIDTH)
            .map(|(i, f)| {
                let l = openness(&f.left_eye, w, h);
                let r = openness(&f.right_eye, w, h);
                let measured: Vec<f32> = [l, r].into_iter().flatten().collect();
                let closed = !measured.is_empty() && measured.iter().all(|o| *o < CLOSED_OPENNESS);
                EyeState { face: i, left_openness: l, right_openness: r, closed }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::geometry::NormRect;
    use image::{DynamicImage, ImageBuffer, Luma};

    fn eye(cx: f32, cy: f32, half_w: f32, half_h: f32) -> Vec<NormPoint> {
        (0..8)
            .map(|i| {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                NormPoint { x: cx + half_w * a.cos(), y: cy + half_h * a.sin() }
            })
            .collect()
    }

    #[test]
    fn open_and_closed() {
        let img = AnalysisImage::from_image(DynamicImage::ImageLuma8(ImageBuffer::from_pixel(1000, 1000, Luma([0]))));
        let face = |half_h| Face {
            bounds: NormRect::new(0.4, 0.4, 0.2, 0.2),
            confidence: 1.0,
            roll: None,
            yaw: None,
            pitch: None,
            left_eye: eye(0.55, 0.45, 0.02, half_h),
            right_eye: eye(0.45, 0.45, 0.02, half_h),
        };
        let r = AspectRatioEyes.eyes(&img, &[face(0.007), face(0.001)]).unwrap();
        assert!(!r[0].closed, "{r:?}");
        assert!(r[1].closed, "{r:?}");
    }
}
