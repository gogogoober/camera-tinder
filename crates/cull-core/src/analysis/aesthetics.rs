//! Aesthetics: an overall "is this a nice photo" score, plus a flag for utility shots
//! (screenshots, receipts, documents, whiteboards).
//!
//! No Rust implementation. Apple: Vision `VNCalculateImageAestheticsScoresRequest`
//! (macOS 15 / iOS 18+). Apple doesn't document the range; observed values are about −1…1.

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aesthetics {
    /// Higher is more appealing. Roughly −1…1 with Apple Vision.
    pub overall: f32,
    /// A picture of information rather than a moment (screenshot, document, receipt).
    pub is_utility: bool,
}

pub trait AestheticsScorer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn aesthetics(&self, img: &AnalysisImage) -> Result<Aesthetics>;
}

/// Overall scores below this are flagged as unappealing.
pub const LOW_OVERALL: f32 = -0.3;
