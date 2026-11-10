//! Saliency: where does the eye go? Gives subject regions for photos without faces, used by
//! `subject_focus` (and handy for smart crops later).
//!
//! No Rust implementation. Apple: Vision `VNGenerateAttentionBasedSaliencyImageRequest`.

use serde::{Deserialize, Serialize};

use super::geometry::NormRect;
use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SalientRegion {
    pub bounds: NormRect,
    pub confidence: f32,
}

pub trait SaliencyDetector: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn salient_regions(&self, img: &AnalysisImage) -> Result<Vec<SalientRegion>>;
}
