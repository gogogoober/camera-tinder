//! Triage: for every photo of a plan, which question to ask (see docs/smart-triage.md).
//!
//! - **Sweep**: set aside; shown neutral at the end ("these look bad, rescue any?").
//! - **Pick**: part of a burst or near-duplicate group ("which of these do you want?").
//! - **Judge**: an ordinary swipe.
//!
//! Computed on every request from stored analysis results by small pure functions (issues → route →
//! group → rank), so there is nothing to keep in sync. Bump [`PLAN_VERSION`] whenever routing rules or
//! thresholds change; outcomes are logged against it.

pub mod route;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analysis::grouping::{Candidate, similar_groups};
use crate::analysis::issues::{Issue, IssueKind, issues};
use crate::analysis::ranking::quality_score;
use crate::analysis::{AnalysisKind, GroupingOptions, PhotoAnalysis};
use crate::model::{Photo, PhotoId};
use crate::moments::BURST_GAP_SECS;

use self::route::{hint_kinds, is_accidental, reject_kinds, route_single};

pub const PLAN_VERSION: &str = "triage.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Question {
    Judge,
    Pick,
    Sweep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupKind {
    /// Frames seconds apart.
    Burst,
    /// The same scene again, minutes apart.
    Similar,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageGroup {
    /// The group's earliest photo; the same for every member.
    pub id: PhotoId,
    pub kind: GroupKind,
    pub size: u32,
    /// 0 = best frame of the group.
    pub rank: u32,
    /// ★: the best frame, and it has no reject-level issue (an all-bad group suggests nothing).
    pub suggested: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageItem {
    pub photo_id: PhotoId,
    pub question: Question,
    pub group: Option<TriageGroup>,
    /// Why it is set aside (Sweep), or what is wrong with this frame (Pick). Worst first.
    pub reasons: Vec<IssueKind>,
    /// Every other issue found: badges only.
    pub hints: Vec<IssueKind>,
    /// `quality_score`, kept so outcomes can be compared with what was suggested.
    pub score: Option<f32>,
    /// False until the photo has at least one current analysis result; such a photo is just a Judge.
    pub analyzed: bool,
}

fn has_results(a: &PhotoAnalysis) -> bool {
    AnalysisKind::ALL.into_iter().any(|k| a.has(k))
}

fn single_item(photo: &Photo, analysis: Option<&PhotoAnalysis>, issues: &[Issue]) -> TriageItem {
    let analysis = analysis.filter(|a| has_results(a));
    let (question, reasons) = route_single(issues);
    TriageItem {
        photo_id: photo.id,
        question,
        group: None,
        hints: hint_kinds(issues, &reasons),
        reasons,
        score: analysis.map(quality_score),
        analyzed: analysis.is_some(),
    }
}

/// Burst if every gap between neighbouring frames is within [`BURST_GAP_SECS`], else similar.
fn group_kind(photos: &[&Photo]) -> GroupKind {
    let mut times: Vec<_> = photos.iter().map(|p| p.captured_at).collect();
    times.sort();
    let tight = times.windows(2).all(|w| matches!(w, [Some(a), Some(b)] if (*b - *a).num_seconds() <= BURST_GAP_SECS));
    if tight { GroupKind::Burst } else { GroupKind::Similar }
}

/// Group the photos that can be grouped and turn each member into a Pick.
fn apply_groups(photos: &[Photo], analyses: &HashMap<PhotoId, PhotoAnalysis>, issues: &[Vec<Issue>], items: &mut [TriageItem]) {
    let index_of: HashMap<PhotoId, usize> = photos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    // Accidental shots are swept even inside a burst, so they take no part in grouping.
    let candidates: Vec<Candidate> = photos
        .iter()
        .enumerate()
        .filter(|(i, _)| items[*i].analyzed && !is_accidental(&issues[*i]))
        .map(|(_, p)| Candidate { captured_at: p.captured_at, analysis: &analyses[&p.id] })
        .collect();
    let options = GroupingOptions { burst_gap_secs: Some(BURST_GAP_SECS), ..Default::default() };

    for group in similar_groups(&candidates, &options) {
        let members: Vec<usize> = group.photo_ids.iter().map(|id| index_of[id]).collect(); // best first
        let member_photos: Vec<&Photo> = members.iter().map(|&i| &photos[i]).collect();
        let first = member_photos.iter().min_by_key(|p| (p.captured_at, p.id)).expect("groups have members");
        let (id, kind, size) = (first.id, group_kind(&member_photos), members.len() as u32);
        for (rank, &i) in members.iter().enumerate() {
            let reasons = reject_kinds(&issues[i]);
            items[i].question = Question::Pick;
            items[i].hints = hint_kinds(&issues[i], &reasons);
            items[i].group = Some(TriageGroup { id, kind, size, rank: rank as u32, suggested: rank == 0 && reasons.is_empty() });
            items[i].reasons = reasons;
        }
    }
}

/// One item per photo, in the same order. `analyses` must hold current results only
/// (`PhotoAnalysis::retain_current`); photos without any are plain Judge items.
pub fn build(photos: &[Photo], analyses: &HashMap<PhotoId, PhotoAnalysis>) -> Vec<TriageItem> {
    let issues: Vec<Vec<Issue>> = photos.iter().map(|p| analyses.get(&p.id).map(issues).unwrap_or_default()).collect();
    let mut items: Vec<TriageItem> = photos.iter().zip(&issues).map(|(p, iss)| single_item(p, analyses.get(&p.id), iss)).collect();
    apply_groups(photos, analyses, &issues, &mut items);
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::accidental::AccidentalReason;
    use crate::analysis::{Accidental, Blur, BlurVerdict, PerceptualHash};
    use crate::model::MediaKind;
    use chrono::NaiveDateTime;

    fn photo(id: PhotoId, time: &str) -> Photo {
        Photo {
            id,
            fingerprint: format!("fp{id}"),
            source_id: "s".into(),
            rel_path: format!("{id}.jpg"),
            file_name: format!("{id}.jpg"),
            kind: MediaKind::Jpeg,
            size: 1,
            captured_at: Some(NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M:%S").unwrap()),
            camera: None,
            decision: None,
        }
    }

    /// An analysis with a hash and a blur verdict (`peak` 0–100).
    fn analysis(id: PhotoId, hash: u64, peak: u8, verdict: BlurVerdict) -> PhotoAnalysis {
        let mut a = PhotoAnalysis::new(id);
        a.perceptual_hash = Some(PerceptualHash(hash));
        a.blur = Some(Blur { score: peak, peak, directionality: 0.1, verdict });
        a
    }

    fn sharp(id: PhotoId, hash: u64) -> PhotoAnalysis {
        analysis(id, hash, 80, BlurVerdict::Sharp)
    }
    fn blurry(id: PhotoId, hash: u64) -> PhotoAnalysis {
        analysis(id, hash, 3, BlurVerdict::Soft)
    }

    fn run(photos: &[Photo], analyses: Vec<PhotoAnalysis>) -> Vec<TriageItem> {
        build(photos, &analyses.into_iter().map(|a| (a.photo_id, a)).collect())
    }

    // Different hashes are far apart (> 10 bits), identical ones are the same frame.
    const SCENE_A: u64 = 0;
    const SCENE_B: u64 = u64::MAX;

    #[test]
    fn a_lone_blurry_photo_is_swept_and_a_sharp_one_is_judged() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T11:00:00")];
        let items = run(&photos, vec![blurry(1, SCENE_A), sharp(2, SCENE_B)]);
        assert_eq!((items[0].question, items[0].reasons.clone()), (Question::Sweep, vec![IssueKind::Blurry]));
        assert_eq!((items[1].question, items[1].group.is_none()), (Question::Judge, true));
        assert!(items.iter().all(|i| i.analyzed && i.score.is_some()));
    }

    #[test]
    fn a_burst_is_a_pick_with_the_best_sharp_frame_suggested() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T10:00:01"), photo(3, "2026-01-01T10:00:02")];
        let items = run(&photos, vec![blurry(1, SCENE_A), sharp(2, SCENE_A), analysis(3, SCENE_A, 60, BlurVerdict::Sharp)]);
        assert!(items.iter().all(|i| i.question == Question::Pick));
        let g = |i: usize| items[i].group.clone().unwrap();
        assert_eq!((g(0).id, g(1).id, g(2).id), (1, 1, 1), "group id is the earliest photo");
        assert_eq!((g(0).kind, g(0).size), (GroupKind::Burst, 3));
        assert_eq!((g(1).rank, g(1).suggested), (0, true), "sharpest frame is best and suggested");
        assert_eq!((g(2).rank, g(2).suggested), (1, false));
        assert_eq!((g(0).rank, items[0].reasons.clone()), (2, vec![IssueKind::Blurry]), "weak frame shows why");
    }

    #[test]
    fn a_burst_where_every_frame_is_bad_stays_a_pick_without_a_suggestion() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T10:00:01")];
        let items = run(&photos, vec![blurry(1, SCENE_A), blurry(2, SCENE_A)]);
        assert!(items.iter().all(|i| i.question == Question::Pick && !i.group.as_ref().unwrap().suggested));
    }

    #[test]
    fn an_accidental_frame_is_swept_and_the_rest_of_the_burst_stays() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T10:00:01"), photo(3, "2026-01-01T10:00:02")];
        let mut pocket = sharp(2, SCENE_A);
        pocket.accidental = Some(Accidental { likely: true, reason: Some(AccidentalReason::Dark) });
        let items = run(&photos, vec![sharp(1, SCENE_A), pocket, sharp(3, SCENE_A)]);
        assert_eq!((items[1].question, items[1].reasons.clone()), (Question::Sweep, vec![IssueKind::Accidental]));
        assert!(items[1].group.is_none());
        assert_eq!((items[0].question, items[2].question), (Question::Pick, Question::Pick));
        assert_eq!(items[0].group.as_ref().unwrap().size, 2);
    }

    #[test]
    fn eyes_closed_alone_is_a_hint_not_a_sweep() {
        use crate::analysis::EyeState;
        let photos = [photo(1, "2026-01-01T10:00:00")];
        let mut a = sharp(1, SCENE_A);
        a.eyes = Some(vec![EyeState { face: 0, left_openness: None, right_openness: None, closed: true }]);
        let items = run(&photos, vec![a]);
        assert_eq!(items[0].question, Question::Judge);
        assert_eq!(items[0].hints, vec![IssueKind::EyesClosed]);
    }

    #[test]
    fn the_same_scene_minutes_apart_is_a_similar_group() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T10:05:00")];
        let items = run(&photos, vec![sharp(1, SCENE_A), sharp(2, SCENE_A)]);
        assert!(items.iter().all(|i| i.question == Question::Pick && i.group.as_ref().unwrap().kind == GroupKind::Similar));
    }

    #[test]
    fn frames_seconds_apart_group_even_when_they_look_different() {
        let photos = [photo(1, "2026-01-01T10:00:00"), photo(2, "2026-01-01T10:00:02")];
        let items = run(&photos, vec![sharp(1, SCENE_A), sharp(2, SCENE_B)]);
        assert!(items.iter().all(|i| i.question == Question::Pick), "a burst is a burst");
    }

    #[test]
    fn a_photo_without_results_is_just_a_judge() {
        let photos = [photo(1, "2026-01-01T10:00:00")];
        let items = build(&photos, &HashMap::new());
        assert_eq!((items[0].question, items[0].analyzed, items[0].score), (Question::Judge, false, None));
    }
}
