//! The analysis pipeline: a list of independent per-photo steps.
//!
//! - A [`PhotoStep`] analyses one decoded preview and returns one [`StepResult`] per stored result
//!   kind it owns. Steps never read each other's output; analyzers that depend on each other (faces →
//!   eyes) live inside **one** step and pass results along in code.
//! - [`Pipeline::from_analyzers`] is the registry: adding a step is one `steps.push(...)` line there.
//! - What is stored stays as it was: one row per (photo, kind) with the implementation id that made it.
//!   A step is stale for a photo when any of its outputs is missing or stored with another
//!   implementation id. Old rows of removed steps are left alone and simply never read.
//! - Photos run in parallel (see `CullerService::analyze`); the steps of one photo run one after
//!   another. They are independent, so running them in parallel later only changes [`run_photo`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::{AnalysisImage, AnalysisKind, Analyzers, PhotoAnalysis};
use crate::model::PhotoId;

/// One stored result of one step for one photo.
#[derive(Debug, Clone, PartialEq)]
pub struct StepResult {
    /// The stored key (`analysis.kind`), e.g. `"blur"`.
    pub kind: String,
    /// Implementation id of the engine that produced it, e.g. `"rust.blur.v1"`.
    pub implementation: String,
    /// The result as JSON (`Null` = computed, nothing to store), or the error message.
    pub result: std::result::Result<Value, String>,
}

/// A unit of per-photo analysis.
pub trait PhotoStep: Send + Sync {
    /// Stable id, for logs ("blur", "subjects").
    fn id(&self) -> &'static str;
    /// Every `(kind, implementation)` this step writes. A photo needs the step when any of them
    /// is missing or stored with a different implementation.
    fn outputs(&self) -> Vec<(String, String)>;
    /// Compute. Returns one [`StepResult`] per output, errors included; never panics on bad input.
    fn run(&self, photo_id: PhotoId, img: &AnalysisImage) -> Vec<StepResult>;
}

/// The registered steps, in run order.
#[derive(Clone, Default)]
pub struct Pipeline {
    pub steps: Vec<Arc<dyn PhotoStep>>,
}

/// Kinds that depend on each other, so they run as one step.
const SUBJECT_KINDS: [AnalysisKind; 5] =
    [AnalysisKind::Faces, AnalysisKind::Eyes, AnalysisKind::FaceFraming, AnalysisKind::Saliency, AnalysisKind::SubjectFocus];

/// Convert what `Analyzers::run` produced for `kinds` into step results.
fn to_results(a: &PhotoAnalysis, kinds: &[AnalysisKind]) -> Vec<StepResult> {
    kinds
        .iter()
        .filter_map(|kind| {
            let implementation = a.implementations.get(kind)?.clone();
            let result = match a.errors.get(kind) {
                Some(e) => Err(e.clone()),
                None => Ok(a.result_json(*kind).unwrap_or(Value::Null)),
            };
            Some(StepResult { kind: kind.as_str().to_string(), implementation, result })
        })
        .collect()
}

/// One analyzer = one step (blur, exposure, …).
struct KindStep {
    analyzers: Arc<Analyzers>,
    kind: AnalysisKind,
}

impl PhotoStep for KindStep {
    fn id(&self) -> &'static str {
        self.kind.as_str()
    }
    fn outputs(&self) -> Vec<(String, String)> {
        self.analyzers.implementation(self.kind).map(|i| vec![(self.kind.as_str().to_string(), i.to_string())]).unwrap_or_default()
    }
    fn run(&self, photo_id: PhotoId, img: &AnalysisImage) -> Vec<StepResult> {
        to_results(&self.analyzers.run(photo_id, img, &[self.kind], None), &[self.kind])
    }
}

/// Faces → eyes and framing; saliency → subject focus. One call, five stored results.
struct SubjectsStep {
    analyzers: Arc<Analyzers>,
}

impl SubjectsStep {
    fn kinds(&self) -> Vec<AnalysisKind> {
        SUBJECT_KINDS.into_iter().filter(|k| self.analyzers.implementation(*k).is_some()).collect()
    }
}

impl PhotoStep for SubjectsStep {
    fn id(&self) -> &'static str {
        "subjects"
    }
    fn outputs(&self) -> Vec<(String, String)> {
        self.kinds().into_iter().filter_map(|k| Some((k.as_str().to_string(), self.analyzers.implementation(k)?.to_string()))).collect()
    }
    fn run(&self, photo_id: PhotoId, img: &AnalysisImage) -> Vec<StepResult> {
        let kinds = self.kinds();
        to_results(&self.analyzers.run(photo_id, img, &kinds, None), &kinds)
    }
}

impl Pipeline {
    /// The registry. Add a step with one `steps.push(...)` line.
    pub fn from_analyzers(analyzers: Arc<Analyzers>) -> Self {
        use AnalysisKind as K;
        let mut steps: Vec<Arc<dyn PhotoStep>> = Vec::new();
        for kind in [K::Blur, K::Exposure, K::Contrast, K::Noise, K::ColorCast, K::Accidental, K::PerceptualHash, K::Similarity, K::FaceQuality, K::Aesthetics, K::Horizon] {
            if analyzers.implementation(kind).is_some() {
                steps.push(Arc::new(KindStep { analyzers: analyzers.clone(), kind }));
            }
        }
        let subjects = SubjectsStep { analyzers };
        if !subjects.kinds().is_empty() {
            steps.push(Arc::new(subjects));
        }
        Self { steps }
    }

    /// Only the steps that write one of `kinds` (all steps when `kinds` is empty).
    pub fn only(mut self, kinds: &[AnalysisKind]) -> Self {
        if !kinds.is_empty() {
            self.steps.retain(|s| s.outputs().iter().any(|(k, _)| kinds.iter().any(|w| w.as_str() == k)));
        }
        self
    }
}

/// Indexes of the steps that `stored` (kind → implementation, for one photo) doesn't satisfy.
pub fn stale_steps(pipeline: &Pipeline, stored: Option<&HashMap<String, String>>, force: bool) -> Vec<usize> {
    pipeline
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| force || step.outputs().iter().any(|(kind, implementation)| stored.and_then(|s| s.get(kind)) != Some(implementation)))
        .map(|(i, _)| i)
        .collect()
}

/// Run the steps at `steps` in order. Before each one, stop if `stop` is set. Returns the results so
/// far (every finished step is complete) and whether it stopped early.
pub fn run_photo(pipeline: &Pipeline, steps: &[usize], photo_id: PhotoId, img: &AnalysisImage, stop: &AtomicBool) -> (Vec<StepResult>, bool) {
    let mut out = Vec::new();
    for &i in steps {
        if stop.load(Ordering::Relaxed) {
            return (out, true);
        }
        out.extend(pipeline.steps[i].run(photo_id, img));
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Face, FaceDetector, NormRect};
    use ::image::{DynamicImage, ImageBuffer, Rgb};
    use std::sync::atomic::AtomicUsize;

    struct Fake {
        id: &'static str,
        kind: &'static str,
        implementation: &'static str,
        runs: Arc<AtomicUsize>,
    }
    impl PhotoStep for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn outputs(&self) -> Vec<(String, String)> {
            vec![(self.kind.into(), self.implementation.into())]
        }
        fn run(&self, _: PhotoId, _: &AnalysisImage) -> Vec<StepResult> {
            self.runs.fetch_add(1, Ordering::Relaxed);
            vec![StepResult { kind: self.kind.into(), implementation: self.implementation.into(), result: Ok(Value::Bool(true)) }]
        }
    }

    fn fake(id: &'static str, implementation: &'static str) -> (Arc<dyn PhotoStep>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        (Arc::new(Fake { id, kind: id, implementation, runs: runs.clone() }), runs)
    }

    fn stored(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn img() -> AnalysisImage {
        AnalysisImage::from_image(DynamicImage::ImageRgb8(ImageBuffer::from_fn(320, 240, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 100]))))
    }

    #[test]
    fn staleness_follows_stored_implementations() {
        let p = Pipeline { steps: vec![fake("a", "a.v1").0, fake("b", "b.v1").0] };
        assert_eq!(stale_steps(&p, None, false), vec![0, 1], "nothing stored");
        assert!(stale_steps(&p, Some(&stored(&[("a", "a.v1"), ("b", "b.v1")])), false).is_empty(), "all current");
        assert_eq!(stale_steps(&p, Some(&stored(&[("a", "a.v1"), ("b", "b.v0")])), false), vec![1], "changed implementation");
        assert_eq!(stale_steps(&p, Some(&stored(&[("a", "a.v1")])), false), vec![1], "missing result");
        assert_eq!(stale_steps(&p, Some(&stored(&[("a", "a.v1"), ("b", "b.v1"), ("old", "x")])), false), Vec::<usize>::new(), "unknown stored kinds are ignored");
        assert_eq!(stale_steps(&p, Some(&stored(&[("a", "a.v1"), ("b", "b.v1")])), true), vec![0, 1], "force");
    }

    #[test]
    fn a_new_step_is_stale_for_already_analysed_photos_only_for_itself() {
        let mut p = Pipeline { steps: vec![fake("a", "a.v1").0] };
        let done = stored(&[("a", "a.v1")]);
        assert!(stale_steps(&p, Some(&done), false).is_empty());
        p.steps.push(fake("c", "c.v1").0); // registering a step = one push
        assert_eq!(stale_steps(&p, Some(&done), false), vec![1]);
    }

    #[test]
    fn run_photo_runs_in_order_and_stops_before_the_next_step() {
        let (a, a_runs) = fake("a", "a.v1");
        let (b, b_runs) = fake("b", "b.v1");
        let p = Pipeline { steps: vec![a, b] };
        let stop = AtomicBool::new(false);
        let (r, stopped) = run_photo(&p, &[0, 1], 1, &img(), &stop);
        assert!(!stopped);
        assert_eq!(r.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(), ["a", "b"]);

        stop.store(true, Ordering::Relaxed);
        let (r, stopped) = run_photo(&p, &[0, 1], 1, &img(), &stop);
        assert!(stopped && r.is_empty());
        assert_eq!((a_runs.load(Ordering::Relaxed), b_runs.load(Ordering::Relaxed)), (1, 1), "no step ran after stop");
    }

    #[test]
    fn rust_pipeline_has_one_step_per_available_analyzer_and_subjects_together() {
        let analyzers = Arc::new(Analyzers::rust());
        let p = Pipeline::from_analyzers(analyzers.clone());
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id()).collect();
        assert!(ids.contains(&"blur") && ids.contains(&"subjects"), "{ids:?}");
        // Every available kind is written by exactly one step.
        for kind in analyzers.available() {
            let writers = p.steps.iter().filter(|s| s.outputs().iter().any(|(k, _)| k == kind.as_str())).count();
            assert_eq!(writers, 1, "{kind:?}");
        }
        let r = run_photo(&p, &(0..p.steps.len()).collect::<Vec<_>>(), 1, &img(), &AtomicBool::new(false)).0;
        assert!(r.iter().all(|x| x.result.is_ok()), "{r:?}");
        assert_eq!(r.len(), analyzers.available().len());
    }

    struct FakeFaces;
    impl FaceDetector for FakeFaces {
        fn implementation(&self) -> &'static str {
            "test.faces"
        }
        fn faces(&self, _: &AnalysisImage) -> crate::Result<Vec<Face>> {
            Ok(vec![Face { bounds: NormRect::new(0.0, 0.3, 0.2, 0.3), confidence: 0.9, roll: None, yaw: None, pitch: None, left_eye: vec![], right_eye: vec![] }])
        }
    }

    #[test]
    fn subjects_step_returns_faces_eyes_framing_and_focus_from_one_call() {
        let mut a = Analyzers::rust();
        a.faces = Some(Arc::new(FakeFaces));
        let p = Pipeline::from_analyzers(Arc::new(a)).only(&[AnalysisKind::FaceFraming]);
        assert_eq!(p.steps.len(), 1, "only the subjects step writes faceFraming");
        let r = p.steps[0].run(1, &img());
        let kinds: Vec<&str> = r.iter().map(|x| x.kind.as_str()).collect();
        for k in ["faces", "eyes", "faceFraming", "subjectFocus"] {
            assert!(kinds.contains(&k), "{k} missing from {kinds:?}");
        }
        assert!(r.iter().all(|x| x.result.is_ok()), "{r:?}");
    }
}
