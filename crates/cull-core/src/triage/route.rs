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

/// A lone photo is set aside only when one of its reject-level issues is at least this severe.
pub const SWEEP_MIN_SEVERITY: f32 = 0.4;

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
    let severe = issues.iter().any(|i| REJECT_KINDS.contains(&i.kind) && i.severity >= SWEEP_MIN_SEVERITY);
    if severe { (Question::Sweep, reject_kinds(issues)) } else { (Question::Judge, Vec::new()) }
}

pub fn is_accidental(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.kind == IssueKind::Accidental)
}
