//! Near-duplicate grouping: "these 6 photos are basically the same shot — keep the best".
//!
//! Exact duplicates (same bytes) already collapse into one photo via the content fingerprint.
//! This groups photos that *look* the same: bursts, retakes, slight reframes.
//!
//! Two photos are linked when they were taken close in time **and** look alike:
//! - with a *decisive* embedding (Apple Vision feature print) from the same model: the
//!   embedding distance alone decides. dHash is unreliable on smooth, low-detail photos
//!   (their hashes mostly encode vignetting, so unrelated skies/water/macros collide);
//! - with a weak embedding (Rust colour layout): embedding **and** hash must both agree;
//! - with hashes only: hash distance under `max_hash_bits`.
//!
//! Linked photos are merged transitively (union-find) into groups; each group is ordered
//! best-first by [`ranking::quality_score`](super::ranking::quality_score).

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use super::PhotoAnalysis;
use super::hash::{NEAR_DUPLICATE_BITS, SAME_SCENE_BITS};
use super::ranking::quality_score;
use super::similarity::{is_decisive, near_duplicate_distance, same_scene_distance};
use crate::model::PhotoId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingOptions {
    /// Photos further apart in time than this are never grouped. `None` = no limit.
    pub max_gap_secs: Option<i64>,
    /// Perceptual hash distance (0–64) that links two photos when no embedding is available.
    pub max_hash_bits: u32,
    /// Use similarity embeddings when both photos have one from the same model.
    pub use_embeddings: bool,
    /// Scale the embedding threshold (1.0 = calibrated default, lower = stricter).
    pub embedding_tolerance: f32,
    /// How many following photos (in time order) each photo is compared with.
    pub window: usize,
    /// Frames this close in time are burst candidates. They are linked only if a second,
    /// visual pass says they show the same general scene (looser than a near-duplicate);
    /// completely unrelated frames shot seconds apart stay apart. With no visual data to
    /// compare, time alone links them. `None` = off.
    pub burst_gap_secs: Option<i64>,
}

impl Default for GroupingOptions {
    fn default() -> Self {
        Self { max_gap_secs: Some(10 * 60), max_hash_bits: NEAR_DUPLICATE_BITS, use_embeddings: true, embedding_tolerance: 1.0, window: 60, burst_gap_secs: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarGroup {
    /// Best first.
    pub photo_ids: Vec<PhotoId>,
    /// `quality_score` for each entry of `photo_ids`.
    pub scores: Vec<f32>,
    /// Same as `photo_ids[0]`.
    pub best: PhotoId,
}

/// One photo's inputs to grouping.
pub struct Candidate<'a> {
    pub captured_at: Option<NaiveDateTime>,
    pub analysis: &'a PhotoAnalysis,
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parent[r] != r {
        r = parent[r];
    }
    let mut i = i;
    while parent[i] != r {
        let next = parent[i];
        parent[i] = r;
        i = next;
    }
    r
}

/// Second pass for burst candidates: do the two frames show roughly the same scene?
/// Unknown (nothing comparable) counts as yes: time is then the only evidence there is.
fn same_general_scene(a: &Candidate, b: &Candidate, opts: &GroupingOptions) -> bool {
    let hash_ok = match (&a.analysis.perceptual_hash, &b.analysis.perceptual_hash) {
        (Some(x), Some(y)) => Some(x.distance(y) <= SAME_SCENE_BITS),
        _ => None,
    };
    if opts.use_embeddings
        && let (Some(x), Some(y)) = (&a.analysis.similarity, &b.analysis.similarity)
        && let Some(d) = x.distance(y)
    {
        let close = d <= same_scene_distance(&x.model) * opts.embedding_tolerance;
        return if is_decisive(&x.model) { close } else { close && hash_ok != Some(false) };
    }
    hash_ok.unwrap_or(true)
}

fn linked(a: &Candidate, b: &Candidate, opts: &GroupingOptions) -> bool {
    if let (Some(burst), Some(x), Some(y)) = (opts.burst_gap_secs, a.captured_at, b.captured_at)
        && (y - x).num_seconds().abs() <= burst
        && same_general_scene(a, b, opts)
    {
        return true;
    }
    if let Some(max) = opts.max_gap_secs {
        match (a.captured_at, b.captured_at) {
            (Some(x), Some(y)) if (y - x).num_seconds().abs() > max => return false,
            (Some(_), None) | (None, Some(_)) => return false,
            _ => {}
        }
    }
    let hash_close = match (&a.analysis.perceptual_hash, &b.analysis.perceptual_hash) {
        (Some(x), Some(y)) => Some(x.distance(y) <= opts.max_hash_bits),
        _ => None,
    };
    if opts.use_embeddings
        && let (Some(x), Some(y)) = (&a.analysis.similarity, &b.analysis.similarity)
        && let Some(d) = x.distance(y)
    {
        let close = d <= near_duplicate_distance(&x.model) * opts.embedding_tolerance;
        return if is_decisive(&x.model) { close } else { close && hash_close != Some(false) };
    }
    hash_close == Some(true)
}

/// Groups of 2+ similar photos. `candidates` must be in capture-time order (as `plan` returns).
pub fn similar_groups(candidates: &[Candidate], opts: &GroupingOptions) -> Vec<SimilarGroup> {
    let n = candidates.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in i + 1..n.min(i + 1 + opts.window) {
            if linked(&candidates[i], &candidates[j], opts) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[b] = a;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    groups
        .into_values()
        .filter(|g| g.len() >= 2)
        .map(|g| {
            let mut scored: Vec<(PhotoId, f32)> = g.iter().map(|i| (candidates[*i].analysis.photo_id, quality_score(candidates[*i].analysis))).collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            SimilarGroup { best: scored[0].0, photo_ids: scored.iter().map(|s| s.0).collect(), scores: scored.iter().map(|s| s.1).collect() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Blur, BlurVerdict, Embedding, PerceptualHash};

    fn t(s: &str) -> Option<NaiveDateTime> {
        Some(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap())
    }

    fn a(id: PhotoId, hash: u64, peak: u8) -> PhotoAnalysis {
        let mut a = PhotoAnalysis::new(id);
        a.perceptual_hash = Some(PerceptualHash(hash));
        a.blur = Some(Blur { score: peak, peak, directionality: 0.1, verdict: BlurVerdict::Sharp });
        a
    }

    #[test]
    fn groups_by_hash_and_time_best_first() {
        let photos = [a(1, 0b0000, 50), a(2, 0b0011, 90), a(3, u64::MAX, 50), a(4, 0b0001, 60), a(5, 0, 50)];
        let times = [t("2026-01-01T10:00:00"), t("2026-01-01T10:00:01"), t("2026-01-01T10:00:02"), t("2026-01-01T10:00:03"), t("2026-01-01T12:00:00")];
        let c: Vec<Candidate> = photos.iter().zip(times).map(|(analysis, captured_at)| Candidate { captured_at, analysis }).collect();
        let g = similar_groups(&c, &GroupingOptions::default());
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].photo_ids, vec![2, 4, 1]);
        assert_eq!(g[0].best, 2);
        // Without a time limit, photo 5 joins.
        let g = similar_groups(&c, &GroupingOptions { max_gap_secs: None, ..Default::default() });
        assert_eq!(g[0].photo_ids.len(), 4);
    }

    #[test]
    fn burst_gap_links_frames_seconds_apart_only_if_they_look_related() {
        // 1 and 2: different scenes, 2 s apart. 4 and 5: same scene (few bits apart), 2 s apart.
        let photos = [a(1, 0, 50), a(2, u64::MAX, 50), a(3, 0x00FF_00FF_00FF_00FF, 50), a(4, 0, 50), a(5, 0b1111, 60)];
        let times = [t("2026-01-01T10:00:00"), t("2026-01-01T10:00:02"), t("2026-01-01T10:09:00"), t("2026-01-01T10:20:00"), t("2026-01-01T10:20:02")];
        let c: Vec<Candidate> = photos.iter().zip(times).map(|(analysis, captured_at)| Candidate { captured_at, analysis }).collect();
        let g = similar_groups(&c, &GroupingOptions { burst_gap_secs: Some(3), ..Default::default() });
        assert_eq!(g.len(), 1, "unrelated frames seconds apart must not group: {g:?}");
        assert_eq!(g[0].photo_ids.len(), 2);
        assert!(g[0].photo_ids.contains(&4) && g[0].photo_ids.contains(&5));
    }

    #[test]
    fn embeddings_override_hash() {
        let mut x = a(1, 0, 50);
        let mut y = a(2, 0, 50); // identical hash
        x.similarity = Some(Embedding { model: "rust.color_layout.v1".into(), vector: vec![1.0, 0.0] });
        y.similarity = Some(Embedding { model: "rust.color_layout.v1".into(), vector: vec![0.0, 1.0] }); // embedding says "different"
        let c = [Candidate { captured_at: None, analysis: &x }, Candidate { captured_at: None, analysis: &y }];
        assert!(similar_groups(&c, &GroupingOptions::default()).is_empty());
    }
}
