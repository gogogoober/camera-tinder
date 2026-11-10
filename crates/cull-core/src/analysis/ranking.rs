//! Ranking: a single "how good is this frame" number, for picking the best of a group.
//!
//! Only meaningful between photos of the same scene (a burst, a near-duplicate group): it
//! rewards sharpness, open eyes, good faces and aesthetics, and penalises every issue.
//! Missing results simply don't contribute.

use super::PhotoAnalysis;
use super::issues::{IssueKind, issues};

/// Higher is better; roughly 0–1, can go negative for very bad frames.
pub fn quality_score(a: &PhotoAnalysis) -> f32 {
    let mut score = 0.5;
    if let Some(b) = &a.blur {
        score += 0.25 * (b.peak as f32 / 100.0) + 0.05 * (b.score as f32 / 100.0);
    }
    if let Some(f) = &a.subject_focus
        && let Some(s) = f.subject
    {
        score += 0.15 * (s as f32 / 100.0);
    }
    if let Some(q) = &a.face_quality
        && !q.is_empty()
    {
        score += 0.15 * q.iter().map(|f| f.quality).sum::<f32>() / q.len() as f32;
    }
    if let Some(s) = &a.aesthetics {
        score += 0.15 * s.overall.clamp(-1.0, 1.0);
    }
    for i in issues(a) {
        let weight = match i.kind {
            IssueKind::EyesClosed | IssueKind::Accidental => 0.4,
            IssueKind::Blurry | IssueKind::MotionBlur | IssueKind::SubjectOutOfFocus => 0.25,
            IssueKind::FaceCutOff | IssueKind::Underexposed | IssueKind::Overexposed | IssueKind::Utility => 0.15,
            _ => 0.05,
        };
        score -= weight * i.severity;
    }
    score
}
