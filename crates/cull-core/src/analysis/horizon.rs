//! Horizon: is the photo tilted?
//!
//! No Rust implementation. Apple: Vision `VNDetectHorizonRequest`, which reports the angle of
//! the horizon (or nothing when there isn't a clear one).

use serde::{Deserialize, Serialize};

use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Horizon {
    /// A horizon line was found.
    pub detected: bool,
    /// Tilt in degrees, positive = counter-clockwise. 0 when not detected.
    pub angle_degrees: f32,
    /// Engine confidence 0–1 (1 when the engine doesn't report one).
    pub confidence: f32,
}

pub trait HorizonDetector: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn horizon(&self, img: &AnalysisImage) -> Result<Horizon>;
}

/// Tilts between these magnitudes look accidental (bigger ones are usually deliberate).
pub const TILT_MIN_DEGREES: f32 = 2.0;
pub const TILT_MAX_DEGREES: f32 = 12.0;
/// Horizons found with less confidence than this are ignored.
pub const MIN_CONFIDENCE: f32 = 0.5;
