//! Issues: turn raw analysis results into plain reject reasons ("eyes closed", "blurry").
//!
//! This is the layer the UI's "problem tabs" read. It only looks at a [`PhotoAnalysis`], so
//! it works the same whichever engines produced the results. Thresholds live next to each
//! feature (e.g. `blur::SOFT_PEAK`); this file only maps verdicts to issues.

use serde::{Deserialize, Serialize};

use super::{BlurVerdict, ExposureVerdict, FocusVerdict, NoiseLevel, PhotoAnalysis, aesthetics, horizon};
use crate::model::PhotoId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueKind {
    Blurry,
    MotionBlur,
    SubjectOutOfFocus,
    Underexposed,
    Overexposed,
    Flat,
    Noisy,
    ColorCast,
    EyesClosed,
    FaceCutOff,
    PoorFaceQuality,
    Tilted,
    LowAesthetic,
    Utility,
    Accidental,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub kind: IssueKind,
    /// 0 (borderline) – 1 (certain / severe). Use it to sort, not as a probability.
    pub severity: f32,
    /// Short human-readable explanation, e.g. "2 of 3 faces have eyes closed".
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoIssues {
    pub photo_id: PhotoId,
    pub issues: Vec<Issue>,
}

/// Faces scoring below this capture quality are flagged (Vision's scale, 0–1).
pub const POOR_FACE_QUALITY: f32 = 0.25;

fn issue(kind: IssueKind, severity: f32, detail: impl Into<String>) -> Issue {
    Issue { kind, severity: severity.clamp(0.0, 1.0), detail: detail.into() }
}

/// Every issue found in `a`, most severe first.
pub fn issues(a: &PhotoAnalysis) -> Vec<Issue> {
    let mut out = Vec::new();

    if let Some(b) = &a.blur {
        let sev = 1.0 - b.peak as f32 / super::blur::SOFT_PEAK as f32;
        match b.verdict {
            BlurVerdict::Sharp => {}
            // The edges are smeared (a swung camera), whether or not some tiles still look sharp over busy texture.
            BlurVerdict::MotionBlur if b.peak >= super::blur::SOFT_PEAK || b.edge_sharpness.is_some_and(|f| f < super::blur::SMEARED_EDGE_FRACTION * 1.25) => {
                out.push(issue(IssueKind::MotionBlur, 0.8, "Edges smeared (camera swung)"))
            }
            BlurVerdict::Soft => out.push(issue(IssueKind::Blurry, 0.4 + sev * 0.6, format!("Nothing sharp (peak {} / 100)", b.peak))),
            BlurVerdict::MotionBlur => out.push(issue(IssueKind::MotionBlur, 0.4 + sev * 0.6, format!("Motion blur (peak {} / 100)", b.peak))),
        }
    }
    if let Some(f) = &a.subject_focus {
        match f.verdict {
            FocusVerdict::BackgroundFocused => out.push(issue(IssueKind::SubjectOutOfFocus, 0.8, "Focus landed on the background")),
            FocusVerdict::SubjectSoft => out.push(issue(IssueKind::SubjectOutOfFocus, 0.5, "Subject is soft")),
            _ => {}
        }
    }
    if let Some(e) = &a.exposure {
        match e.verdict {
            ExposureVerdict::Underexposed => out.push(issue(IssueKind::Underexposed, 0.3 + (0.25 - e.mean).max(0.0) * 3.0, format!("Dark (mean {:.0}%)", e.mean * 100.0))),
            ExposureVerdict::Overexposed => {
                out.push(issue(IssueKind::Overexposed, 0.3 + e.highlights_clipped * 2.0, format!("{:.0}% blown highlights", e.highlights_clipped * 100.0)))
            }
            ExposureVerdict::Ok => {}
        }
    }
    if let Some(c) = &a.contrast
        && c.flat
    {
        out.push(issue(IssueKind::Flat, 0.3, "Flat / hazy"));
    }
    if let Some(n) = &a.noise {
        match n.level {
            NoiseLevel::High => out.push(issue(IssueKind::Noisy, 0.5, "Very grainy")),
            NoiseLevel::Medium => out.push(issue(IssueKind::Noisy, 0.2, "Grainy")),
            NoiseLevel::Low => {}
        }
    }
    if let Some(c) = &a.color_cast
        && c.hue != super::CastHue::Neutral
    {
        out.push(issue(IssueKind::ColorCast, (c.strength - super::color::CAST_STRENGTH) * 2.0 + 0.1, format!("{:?} colour cast", c.hue)));
    }
    if let Some(eyes) = &a.eyes {
        let closed = eyes.iter().filter(|e| e.closed).count();
        if closed > 0 {
            out.push(issue(IssueKind::EyesClosed, 0.5 + 0.5 * closed as f32 / eyes.len() as f32, format!("{closed} of {} faces have eyes closed", eyes.len())));
        }
    }
    if let Some(f) = &a.face_framing
        && !f.cut_faces.is_empty()
    {
        out.push(issue(IssueKind::FaceCutOff, 0.4, format!("{} face(s) cut off by the frame", f.cut_faces.len())));
    }
    if let Some(q) = &a.face_quality
        && let Some(best) = q.iter().map(|f| f.quality).reduce(f32::max)
        && best < POOR_FACE_QUALITY
    {
        out.push(issue(IssueKind::PoorFaceQuality, 0.3 + (POOR_FACE_QUALITY - best), format!("Best face quality {:.2}", best)));
    }
    if let Some(h) = &a.horizon
        && h.detected
        && h.confidence >= horizon::MIN_CONFIDENCE
        && (horizon::TILT_MIN_DEGREES..=horizon::TILT_MAX_DEGREES).contains(&h.angle_degrees.abs())
    {
        out.push(issue(IssueKind::Tilted, h.angle_degrees.abs() / 10.0, format!("Horizon tilted {:.1}°", h.angle_degrees)));
    }
    if let Some(s) = &a.aesthetics {
        if s.is_utility {
            out.push(issue(IssueKind::Utility, 0.6, "Looks like a screenshot / document"));
        }
        if s.overall < aesthetics::LOW_OVERALL {
            out.push(issue(IssueKind::LowAesthetic, (aesthetics::LOW_OVERALL - s.overall) + 0.3, format!("Aesthetic score {:.2}", s.overall)));
        }
    }
    if let Some(acc) = &a.accidental
        && acc.likely
    {
        out.push(issue(IssueKind::Accidental, 0.9, format!("Accidental shot ({:?})", acc.reason.unwrap())));
    }

    out.sort_by(|a, b| b.severity.total_cmp(&a.severity));
    out
}
