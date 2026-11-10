//! Faces: where are the faces, and where are their eyes?
//!
//! No Rust implementation ships today (pure-Rust detectors either need nightly or give no
//! landmarks). The Apple implementation (`cull-platform-apple`, Vision
//! `VNDetectFaceLandmarksRequest`) fills this in on macOS/iOS. Other platforms can plug in an
//! ONNX model later behind the same trait.
//!
//! Downstream features — `eyes`, `framing`, `subject_focus` — consume [`Face`] values and stay
//! engine-independent.

use serde::{Deserialize, Serialize};

use super::geometry::{NormPoint, NormRect};
use super::image::AnalysisImage;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    /// Face box in the upright image (normalised, top-left origin).
    pub bounds: NormRect,
    /// Detector confidence 0–1.
    pub confidence: f32,
    /// Head rotation in degrees when the engine reports it.
    pub roll: Option<f32>,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    /// Outline of each eye in image coordinates (normalised, top-left origin). The subject's
    /// left eye appears on the image's right. Empty when landmarks were not found.
    pub left_eye: Vec<NormPoint>,
    pub right_eye: Vec<NormPoint>,
}

pub trait FaceDetector: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn faces(&self, img: &AnalysisImage) -> Result<Vec<Face>>;
}
