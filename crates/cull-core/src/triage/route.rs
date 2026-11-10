//! Which question each photo gets, from its issues alone. The thresholds live here, in one place.

use crate::analysis::issues::{Issue, IssueKind};

use super::Question;

/// Issues bad enough that the photo may be set aside (Sweep), or, inside a group, that mark a weak frame.
/// Eyes closed is deliberately not here (candids): it is only a hint and a ranking signal.
pub const REJECT_KINDS: &[IssueKind] = &[
    IssueKind::Blurry,
    IssueKind::MotionBlur,
    IssueKind::SubjectOutOfFocus,
    IssueKind::Underexposed,
    IssueKind::Overexposed,
    IssueKind::FaceCutOff,
    IssueKind::Utility,
];

/// What can set a lone photo aside (Sweep): accidental shots, and photos so blurred nothing is usable.
/// Everything else in [`REJECT_KINDS`] (exposure, subject focus, screenshot-looking, cut-off faces) is an
/// artistic or uncertain call: it stays a Judge with a badge and only lowers the rank inside a group.
pub const SWEEP_BLUR_KINDS: &[IssueKind] = &[IssueKind::Blurry, IssueKind::MotionBlur];

/// Out-of-focus blur must be at least this severe to sweep (`issues()`: 0.4 = barely soft, 1.0 = nothing sharp anywhere).
pub const SWEEP_MIN_BLUR_SEVERITY: f32 = 0.85;
/// Smear from camera shake sweeps earlier: a deliberate pan keeps a sharp subject (so it is not flagged at all),
/// while a shaken frame has no sharp region anywhere (best-region peak about 25 / 100 or less).
pub const SWEEP_MIN_MOTION_BLUR_SEVERITY: f32 = 0.55;

/// The reject-level issues of a photo, worst first (`issues()` already sorts by severity).
pub fn reject_kinds(issues: &[Issue]) -> Vec<IssueKind> {
    issues.iter().map(|i| i.kind).filter(|k| REJECT_KINDS.contains(k)).collect()
}

/// Every other issue, shown as a badge. Never a question on its own.
pub fn hint_kinds(issues: &[Issue], reasons: &[IssueKind]) -> Vec<IssueKind> {
    issues.iter().map(|i| i.kind).filter(|k| !reasons.contains(k)).collect()
}

/// The question for a photo that is *not* in a group, with its reasons.
/// Accidental shots are swept even when they sit inside a burst (the caller checks that first).
pub fn route_single(issues: &[Issue]) -> (Question, Vec<IssueKind>) {
    if is_accidental(issues) {
        return (Question::Sweep, vec![IssueKind::Accidental]);
    }
    let ruined: Vec<IssueKind> = issues
        .iter()
        .filter(|i| match i.kind {
            IssueKind::Blurry => i.severity >= SWEEP_MIN_BLUR_SEVERITY,
            IssueKind::MotionBlur => i.severity >= SWEEP_MIN_MOTION_BLUR_SEVERITY,
            _ => false,
        })
        .map(|i| i.kind)
        .collect();
    if ruined.is_empty() { (Question::Judge, Vec::new()) } else { (Question::Sweep, ruined) }
}

pub fn is_accidental(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.kind == IssueKind::Accidental)
}
