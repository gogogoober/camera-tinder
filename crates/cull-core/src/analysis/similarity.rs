//! Similarity embedding: a vector describing what is *in* the photo, for near-duplicate
//! grouping that survives reframing, zoom and small moments (a blink, a step).
//!
//! Embeddings from different implementations are not comparable: [`Embedding::distance`]
//! returns `None` unless both came from the same `model`. Each model has its own
//! near-duplicate threshold, see [`near_duplicate_distance`].
//!
//! Implementations:
//! - `rust.color_layout.v1` (here): an 8×8 colour thumbnail, normalised. Cheap, works
//!   everywhere, roughly as strong as the perceptual hash.
//! - `apple.vision.feature_print.*` (cull-platform-apple): Vision's image feature print.

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Embedding {
    /// Which implementation (and revision) made this vector.
    pub model: String,
    pub vector: Vec<f32>,
}

impl Embedding {
    /// Euclidean distance; `None` when the two came from different models.
    pub fn distance(&self, other: &Embedding) -> Option<f32> {
        if self.model != other.model || self.vector.len() != other.vector.len() {
            return None;
        }
        Some(self.vector.iter().zip(&other.vector).map(|(a, b)| (a - b) * (a - b)).sum::<f32>().sqrt())
    }
}

pub trait SimilarityEmbedder: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn embed(&self, img: &AnalysisImage) -> Result<Embedding>;
}

/// Distance at or below which two frames shot seconds apart still count as the *same general scene*
/// (a burst of a moving subject, a re-framed shot). Looser than [`near_duplicate_distance`] but
/// still under the "unrelated" floor of the same calibration.
pub fn same_scene_distance(model: &str) -> f32 {
    if model.starts_with("rust.color_layout") {
        0.40 // near-duplicates <= 0.27, unrelated >= 0.43
    } else if model.starts_with("apple.vision.feature_print") {
        0.54 // near-duplicates <= 0.29, unrelated >= 0.59
    } else {
        0.15
    }
}

/// Distance at or below which two embeddings from `model` count as near-duplicates.
/// Calibrated per model; unknown models get a conservative value.
pub fn near_duplicate_distance(model: &str) -> f32 {
    if model.starts_with("rust.color_layout") {
        // Same calibration: near-duplicates ≤ 0.27, unrelated ≥ 0.43.
        0.35
    } else if model.starts_with("apple.vision.feature_print") {
        // Revision 2 (macOS 14+) prints are unit-length vectors (distance 0–2).
        // Calibrated on test-photos (3% crop + brightened copies vs 117 unrelated photos):
        // near-duplicates ≤ 0.29, unrelated ≥ 0.59.
        0.45
    } else {
        0.1
    }
}

/// Whether `model`'s distance alone can decide near-duplicates. Weak models (the Rust colour
/// layout) must be confirmed by the perceptual hash: on mostly grey scenes, unrelated photos
/// with a similar light-to-dark layout land close together.
pub fn is_decisive(model: &str) -> bool {
    model.starts_with("apple.vision.feature_print")
}

pub struct ColorLayout;

impl SimilarityEmbedder for ColorLayout {
    fn implementation(&self) -> &'static str {
        "rust.color_layout.v1"
    }

    fn embed(&self, img: &AnalysisImage) -> Result<Embedding> {
        let small = image::imageops::resize(img.working_rgb()?, 8, 8, image::imageops::FilterType::Triangle);
        let mut v: Vec<f32> = small.pixels().flat_map(|p| p.0.map(|c| c as f32)).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        v.iter_mut().for_each(|x| *x -= mean);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(Embedding { model: self.implementation().into(), vector: v })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn scene(shift: i32, flip: bool) -> AnalysisImage {
        let img = ImageBuffer::from_fn(320, 240, |x, y| {
            let x = if flip { 319 - x } else { x } as i32 + shift;
            Rgb([(x.clamp(0, 319) * 255 / 320) as u8, (y * 255 / 240) as u8, 90])
        });
        AnalysisImage::from_image(DynamicImage::ImageRgb8(img))
    }

    #[test]
    fn similar_close_different_far_models_incomparable() {
        let a = ColorLayout.embed(&scene(0, false)).unwrap();
        let b = ColorLayout.embed(&scene(8, false)).unwrap();
        let c = ColorLayout.embed(&scene(0, true)).unwrap();
        let t = near_duplicate_distance(&a.model);
        assert!(a.distance(&b).unwrap() < t);
        assert!(a.distance(&c).unwrap() > t);
        let other = Embedding { model: "x".into(), vector: a.vector.clone() };
        assert_eq!(a.distance(&other), None);
    }
}
