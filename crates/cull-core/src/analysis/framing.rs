//! Face framing: is a face cut off by the edge of the frame?
//!
//! Input: the faces from `faces`. Rust implementation: a face box that reaches the image
//! border is cut. Tiny faces (crowds, background people) are ignored.

use serde::{Deserialize, Serialize};

use super::faces::Face;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceFraming {
    /// Indexes into the photo's face list of faces touching the frame edge.
    pub cut_faces: Vec<usize>,
}

pub trait FramingAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn framing(&self, faces: &[Face]) -> Result<FaceFraming>;
}

/// Distance from the edge (fraction of the image) that counts as touching.
pub const EDGE_MARGIN: f32 = 0.005;
/// Faces narrower than this fraction of the image are background faces.
pub const MIN_FACE_WIDTH: f32 = 0.05;

pub struct EdgeFraming;

impl FramingAnalyzer for EdgeFraming {
    fn implementation(&self) -> &'static str {
        "rust.face_framing.v1"
    }

    fn framing(&self, faces: &[Face]) -> Result<FaceFraming> {
        let cut_faces = faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.bounds.width >= MIN_FACE_WIDTH && f.bounds.touches_edge(EDGE_MARGIN))
            .map(|(i, _)| i)
            .collect();
        Ok(FaceFraming { cut_faces })
    }
}
