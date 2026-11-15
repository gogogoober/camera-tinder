//! Learned taste: a small logistic model of how likely the user is to Keep a photo, fitted from their decisions and
//! the stored analysis (`train`), recorded in `history`, and used by `plan()` to rank a burst's frames once approved.
//! Nothing changes behaviour until a proposal is approved; with none approved, ranking is the hand-tuned score.

pub mod history;
pub mod train;

use serde::{Deserialize, Serialize};

use crate::analysis::PhotoAnalysis;

/// Every feature, in a fixed order. Weights are stored by name, so adding one later keeps old models valid.
pub const BASE_FEATURES: [&str; 25] = [
    "blurScore",
    "blurPeak",
    "blurDirectionality",
    "edgeSharpness",
    "exposureMean",
    "shadowsClipped",
    "highlightsClipped",
    "contrastRms",
    "contrastRange",
    "noiseSigma",
    "castStrength",
    "warmCool",
    "greenMagenta",
    "faceCount",
    "largestFaceArea",
    "eyesClosedShare",
    "faceQuality",
    "cutFaces",
    "saliencyConfidence",
    "subjectFocus",
    "frameFocus",
    "aesthetics",
    "isUtility",
    "horizonTilt",
    "accidentalLikely",
];

/// A photo with at least this many detected faces counts as "people", anything else as "things".
pub const PEOPLE_MIN_FACES: usize = 1;

/// Base features that also get a people-only copy (`people·blurScore`...). The weight of a copy is an
/// adjustment on top of the shared one, so with little data for people photos the shared weight stands alone.
/// Face features are left out: they are already zero or missing for things.
const SPLIT: [usize; 19] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 18, 19, 20, 21, 22, 23];

/// Every feature name: the base ones, then `isPeople`, then the people-only copies.
pub fn names() -> &'static [String] {
    static NAMES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
        BASE_FEATURES
            .iter()
            .map(|n| n.to_string())
            .chain(std::iter::once("isPeople".to_string()))
            .chain(SPLIT.iter().map(|&i| format!("people·{}", BASE_FEATURES[i])))
            .collect()
    });
    &NAMES
}

/// `Some(true)` = people, `Some(false)` = things, `None` = faces were not analysed.
pub fn is_people(a: &PhotoAnalysis) -> Option<bool> {
    a.faces.as_ref().map(|f| f.len() >= PEOPLE_MIN_FACES)
}

/// The raw feature values of one photo, in [`names`] order (`None` = missing or not applicable).
pub fn features(a: &PhotoAnalysis) -> Vec<Option<f32>> {
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    let mut f: Vec<Option<f32>> = vec![None; BASE_FEATURES.len()];
    if let Some(b) = &a.blur {
        f[0] = Some(b.score as f32);
        f[1] = Some(b.peak as f32);
        f[2] = Some(b.directionality);
        f[3] = b.edge_sharpness;
    }
    if let Some(e) = &a.exposure {
        f[4] = Some(e.mean);
        f[5] = Some(e.shadows_clipped);
        f[6] = Some(e.highlights_clipped);
    }
    if let Some(c) = &a.contrast {
        f[7] = Some(c.rms);
        f[8] = Some(c.range);
    }
    f[9] = a.noise.as_ref().map(|n| n.sigma);
    if let Some(c) = &a.color_cast {
        f[10] = Some(c.strength);
        f[11] = Some(c.warm_cool);
        f[12] = Some(c.green_magenta);
    }
    if let Some(faces) = &a.faces {
        f[13] = Some(faces.len() as f32);
        f[14] = Some(faces.iter().map(|x| x.bounds.width * x.bounds.height).fold(0.0, f32::max));
    }
    if let Some(eyes) = &a.eyes
        && !eyes.is_empty()
    {
        f[15] = Some(eyes.iter().filter(|e| e.closed).count() as f32 / eyes.len() as f32);
    }
    if let Some(q) = &a.face_quality
        && !q.is_empty()
    {
        f[16] = Some(q.iter().map(|x| x.quality).sum::<f32>() / q.len() as f32);
    }
    f[17] = a.face_framing.as_ref().map(|x| x.cut_faces.len() as f32);
    if let Some(s) = &a.saliency {
        f[18] = Some(s.iter().map(|r| r.confidence).fold(0.0, f32::max));
    }
    if let Some(s) = &a.subject_focus {
        f[19] = s.subject.map(|v| v as f32);
        f[20] = Some(s.frame as f32);
    }
    if let Some(s) = &a.aesthetics {
        f[21] = Some(s.overall);
        f[22] = Some(flag(s.is_utility));
    }
    if let Some(h) = &a.horizon
        && h.detected
    {
        f[23] = Some(h.angle_degrees.abs());
    }
    f[24] = a.accidental.as_ref().map(|x| flag(x.likely));
    let people = is_people(a);
    f.push(people.map(flag));
    for &i in &SPLIT {
        f.push(if people == Some(true) { f[i] } else { None });
    }
    f
}

/// One feature's weight on the standardized value `(x - mean) / std`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureWeight {
    pub name: String,
    pub weight: f32,
    pub mean: f32,
    pub std: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Weights {
    pub bias: f32,
    pub features: Vec<FeatureWeight>,
}

impl Weights {
    /// Higher = more likely a Keep. A missing feature contributes nothing (it counts as an average photo).
    pub fn score(&self, a: &PhotoAnalysis) -> f32 {
        let values = features(a);
        let mut s = self.bias;
        for fw in &self.features {
            let Some(i) = names().iter().position(|n| *n == fw.name) else { continue };
            if let Some(x) = values[i] {
                s += fw.weight * (x - fw.mean) / fw.std;
            }
        }
        s
    }

    pub fn weight_of(&self, name: &str) -> Option<f32> {
        self.features.iter().find(|f| f.name == name).map(|f| f.weight)
    }
}

/// How one feature's weight moves between two models. `old` is `None` when there was no model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeightChange {
    pub name: String,
    pub old: Option<f32>,
    pub new: f32,
    pub delta: f32,
}

/// Every feature whose weight changes, biggest move first. With no old model, `old` counts as 0.
pub fn weight_changes(old: Option<&Weights>, new: &Weights) -> Vec<WeightChange> {
    let mut out: Vec<WeightChange> = new
        .features
        .iter()
        .map(|f| {
            let before = old.and_then(|o| o.weight_of(&f.name));
            WeightChange { name: f.name.clone(), old: before, new: f.weight, delta: f.weight - before.unwrap_or(0.0) }
        })
        .collect();
    out.sort_by(|a, b| b.delta.abs().total_cmp(&a.delta.abs()));
    out
}
