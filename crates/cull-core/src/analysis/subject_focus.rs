//! Subject focus: is the *subject* sharp, or did focus land on the background?
//!
//! Input: subject regions — faces when there are any, otherwise salient regions. Rust
//! implementation: Laplacian sharpness inside the subject regions versus the whole frame,
//! on the same 640 px working copy (and 0–100 scale) as `blur`, so the numbers compare.

use serde::{Deserialize, Serialize};

use super::geometry::NormRect;
use super::image::AnalysisImage;
use crate::error::Result;
use crate::sharpness::{laplacian_variance, squash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FocusVerdict {
    InFocus,
    /// The subject is soft.
    SubjectSoft,
    /// The subject is soft and the background is clearly sharper: focus missed behind/in front.
    BackgroundFocused,
    /// No subject regions were given.
    NoSubject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectFocus {
    /// Sharpness 0–100 of the sharpest subject region (`None` without subjects).
    pub subject: Option<u8>,
    /// Sharpness 0–100 of the whole frame, for comparison.
    pub frame: u8,
    pub verdict: FocusVerdict,
}

pub trait SubjectFocusAnalyzer: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn subject_focus(&self, img: &AnalysisImage, subjects: &[NormRect]) -> Result<SubjectFocus>;
}

/// Subject sharpness below this is soft (same scale as `blur::SOFT_PEAK`).
pub const SOFT_SUBJECT: u8 = 35;
/// Background this much sharper than the subject means focus landed elsewhere.
pub const BACKGROUND_MARGIN: u8 = 15;

pub struct LaplacianSubjectFocus;

impl SubjectFocusAnalyzer for LaplacianSubjectFocus {
    fn implementation(&self) -> &'static str {
        "rust.subject_focus.v2"
    }

    fn subject_focus(&self, img: &AnalysisImage, subjects: &[NormRect]) -> Result<SubjectFocus> {
        let grey = img.working_grey()?;
        let (w, h) = grey.dimensions();
        let frame = squash(laplacian_variance(grey, 0, 0, w, h));
        let subject = subjects
            .iter()
            .filter_map(|r| {
                let (x, y, rw, rh) = r.to_pixels(w, h);
                (rw >= 6 && rh >= 6).then(|| squash(laplacian_variance(grey, x, y, rw, rh)))
            })
            .max();
        let verdict = match subject {
            None => FocusVerdict::NoSubject,
            Some(s) if s >= SOFT_SUBJECT => FocusVerdict::InFocus,
            Some(s) if frame >= s.saturating_add(BACKGROUND_MARGIN) => FocusVerdict::BackgroundFocused,
            Some(_) => FocusVerdict::SubjectSoft,
        };
        Ok(SubjectFocus { subject, frame, verdict })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma};

    #[test]
    fn soft_subject_sharp_background() {
        // Detailed everywhere except a flat square in the middle (the "subject").
        let img = ImageBuffer::from_fn(400, 400, |x, y| {
            let inside = (150..250).contains(&x) && (150..250).contains(&y);
            Luma([if inside { 128 } else if (x / 3 + y / 3) % 2 == 0 { 30 } else { 220 }])
        });
        let img = AnalysisImage::from_image(DynamicImage::ImageLuma8(img));
        let r = LaplacianSubjectFocus.subject_focus(&img, &[NormRect::new(0.4, 0.4, 0.2, 0.2)]).unwrap();
        assert_eq!(r.verdict, FocusVerdict::BackgroundFocused, "{r:?}");
        let sharp = LaplacianSubjectFocus.subject_focus(&img, &[NormRect::new(0.0, 0.0, 0.3, 0.3)]).unwrap();
        assert_eq!(sharp.verdict, FocusVerdict::InFocus, "{sharp:?}");
        assert_eq!(LaplacianSubjectFocus.subject_focus(&img, &[]).unwrap().verdict, FocusVerdict::NoSubject);
    }
}
