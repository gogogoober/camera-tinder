//! Face capture quality: how good is each face as a capture (sharpness, lighting, pose,
//! expression)? Meant for comparing shots of the same person — "best face in the burst" —
//! not as an absolute grade.
//!
//! No Rust implementation. Apple: Vision `VNDetectFaceCaptureQualityRequest` (0–1).

use serde::{Deserialize, Serialize};

use super::geometry::NormRect;
use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceQuality {
    /// The face this score belongs to (match against `faces` by overlap if needed).
    pub bounds: NormRect,
    /// 0 (poor) – 1 (excellent).
    pub quality: f32,
}

pub trait FaceQualityScorer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn face_quality(&self, img: &AnalysisImage) -> Result<Vec<FaceQuality>>;
}
