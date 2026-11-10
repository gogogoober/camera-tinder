//! Static photo analysis: everything that can make someone reject a photo, measured locally.
//!
//! **Read `docs/photo-analysis.md` first** — it lists every feature, its result type, its
//! implementations and how to call it.
//!
//! Shape of the system:
//! - Each feature lives in its own module with **one trait** (the stable interface), **one
//!   result type**, and zero or more implementations. Swapping an engine (Rust ↔ Apple Vision
//!   ↔ an ONNX model) never changes the result type or callers.
//! - [`Analyzers`] is the registry: one optional slot per feature. `Analyzers::rust()` fills
//!   the pure-Rust ones; platforms add more (`cull_platform_apple::vision::install`).
//! - Every implementation has an `implementation()` id, stored next to its result. When the
//!   id changes, stored results are stale and get recomputed.
//! - Input is always an [`AnalysisImage`] built from the cached master preview, so analysis
//!   never reads the originals.
//! - Higher-level helpers: [`issues`] turns raw results into reject reasons,
//!   [`grouping`] builds near-duplicate groups, [`ranking`] picks the best of a group.

pub mod accidental;
pub mod aesthetics;
pub mod blur;
pub mod color;
pub mod contrast;
pub mod exposure;
pub mod eyes;
pub mod face_quality;
pub mod faces;
pub mod framing;
pub mod geometry;
pub mod grouping;
pub mod hash;
pub mod horizon;
pub mod image;
pub mod issues;
pub mod noise;
pub mod pipeline;
pub mod ranking;
pub mod saliency;
pub mod similarity;
pub mod subject_focus;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use self::accidental::{Accidental, AccidentalAnalyzer, AccidentalReason};
pub use self::aesthetics::{Aesthetics, AestheticsScorer};
pub use self::blur::{Blur, BlurAnalyzer, BlurVerdict};
pub use self::color::{CastHue, ColorCast, ColorCastAnalyzer};
pub use self::contrast::{Contrast, ContrastAnalyzer};
pub use self::exposure::{Exposure, ExposureAnalyzer, ExposureVerdict};
pub use self::eyes::{EyeState, EyeStateAnalyzer};
pub use self::face_quality::{FaceQuality, FaceQualityScorer};
pub use self::faces::{Face, FaceDetector};
pub use self::framing::{FaceFraming, FramingAnalyzer};
pub use self::geometry::{NormPoint, NormRect};
pub use self::grouping::{GroupingOptions, SimilarGroup};
pub use self::hash::{PerceptualHash, PerceptualHasher};
pub use self::horizon::{Horizon, HorizonDetector};
pub use self::image::AnalysisImage;
pub use self::issues::{Issue, IssueKind, PhotoIssues};
pub use self::noise::{Noise, NoiseAnalyzer, NoiseLevel};
pub use self::saliency::{SalientRegion, SaliencyDetector};
pub use self::similarity::{Embedding, SimilarityEmbedder};
pub use self::subject_focus::{FocusVerdict, SubjectFocus, SubjectFocusAnalyzer};
use crate::model::PhotoId;

/// One per feature. Serialised (and stored in SQLite) in camelCase, e.g. `"subjectFocus"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisKind {
    Blur,
    Exposure,
    Contrast,
    Noise,
    ColorCast,
    Accidental,
    PerceptualHash,
    Similarity,
    Faces,
    Eyes,
    FaceFraming,
    FaceQuality,
    Saliency,
    SubjectFocus,
    Aesthetics,
    Horizon,
}

impl AnalysisKind {
    /// Every kind, in the order they run (dependencies first).
    pub const ALL: [AnalysisKind; 16] = [
        Self::Blur,
        Self::Exposure,
        Self::Contrast,
        Self::Noise,
        Self::ColorCast,
        Self::Accidental,
        Self::PerceptualHash,
        Self::Similarity,
        Self::Faces,
        Self::Eyes,
        Self::FaceFraming,
        Self::FaceQuality,
        Self::Saliency,
        Self::SubjectFocus,
        Self::Aesthetics,
        Self::Horizon,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blur => "blur",
            Self::Exposure => "exposure",
            Self::Contrast => "contrast",
            Self::Noise => "noise",
            Self::ColorCast => "colorCast",
            Self::Accidental => "accidental",
            Self::PerceptualHash => "perceptualHash",
            Self::Similarity => "similarity",
            Self::Faces => "faces",
            Self::Eyes => "eyes",
            Self::FaceFraming => "faceFraming",
            Self::FaceQuality => "faceQuality",
            Self::Saliency => "saliency",
            Self::SubjectFocus => "subjectFocus",
            Self::Aesthetics => "aesthetics",
            Self::Horizon => "horizon",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Results this kind consumes. `SubjectFocus` uses whichever of its inputs are available.
    pub fn inputs(self) -> &'static [AnalysisKind] {
        match self {
            Self::Eyes | Self::FaceFraming => &[Self::Faces],
            Self::SubjectFocus => &[Self::Faces, Self::Saliency],
            _ => &[],
        }
    }
}

/// All results for one photo. A field is `None` when that kind hasn't been computed (or
/// failed: see `errors`). `implementations` records which engine produced each result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoAnalysis {
    pub photo_id: PhotoId,
    pub blur: Option<Blur>,
    pub exposure: Option<Exposure>,
    pub contrast: Option<Contrast>,
    pub noise: Option<Noise>,
    pub color_cast: Option<ColorCast>,
    pub accidental: Option<Accidental>,
    pub perceptual_hash: Option<PerceptualHash>,
    pub similarity: Option<Embedding>,
    pub faces: Option<Vec<Face>>,
    pub eyes: Option<Vec<EyeState>>,
    pub face_framing: Option<FaceFraming>,
    pub face_quality: Option<Vec<FaceQuality>>,
    pub saliency: Option<Vec<SalientRegion>>,
    pub subject_focus: Option<SubjectFocus>,
    pub aesthetics: Option<Aesthetics>,
    pub horizon: Option<Horizon>,
    pub implementations: BTreeMap<AnalysisKind, String>,
    pub errors: BTreeMap<AnalysisKind, String>,
}

macro_rules! per_kind {
    ($($kind:ident => $field:ident),* $(,)?) => {
        impl PhotoAnalysis {
            /// The result for `kind` as JSON (how it is stored).
            pub fn result_json(&self, kind: AnalysisKind) -> Option<serde_json::Value> {
                match kind {
                    $(AnalysisKind::$kind => self.$field.as_ref().and_then(|v| serde_json::to_value(v).ok()),)*
                }
            }

            /// Set the result for `kind` from stored JSON.
            pub fn set_result_json(&mut self, kind: AnalysisKind, value: serde_json::Value) -> serde_json::Result<()> {
                match kind {
                    $(AnalysisKind::$kind => self.$field = Some(serde_json::from_value(value)?),)*
                }
                Ok(())
            }

            pub fn has(&self, kind: AnalysisKind) -> bool {
                match kind {
                    $(AnalysisKind::$kind => self.$field.is_some(),)*
                }
            }

            fn clear(&mut self, kind: AnalysisKind) {
                match kind {
                    $(AnalysisKind::$kind => self.$field = None,)*
                }
            }
        }
    };
}

per_kind! {
    Blur => blur,
    Exposure => exposure,
    Contrast => contrast,
    Noise => noise,
    ColorCast => color_cast,
    Accidental => accidental,
    PerceptualHash => perceptual_hash,
    Similarity => similarity,
    Faces => faces,
    Eyes => eyes,
    FaceFraming => face_framing,
    FaceQuality => face_quality,
    Saliency => saliency,
    SubjectFocus => subject_focus,
    Aesthetics => aesthetics,
    Horizon => horizon,
}

impl PhotoAnalysis {
    pub fn new(photo_id: PhotoId) -> Self {
        Self { photo_id, ..Default::default() }
    }

    /// Copy every result (and its implementation/error) present in `other` into `self`.
    pub fn merge(&mut self, other: PhotoAnalysis) {
        for kind in AnalysisKind::ALL {
            if let Some(v) = other.result_json(kind) {
                let _ = self.set_result_json(kind, v);
                self.errors.remove(&kind);
            } else if let Some(e) = other.errors.get(&kind) {
                self.clear(kind);
                self.errors.insert(kind, e.clone());
            }
            if let Some(i) = other.implementations.get(&kind) {
                self.implementations.insert(kind, i.clone());
            }
        }
    }

    /// Forget results whose stored implementation isn't the one registered now (an engine was swapped
    /// or removed). Rows stay in the database; they are just not read. Re-running overwrites them.
    pub fn retain_current(&mut self, analyzers: &Analyzers) {
        for kind in AnalysisKind::ALL {
            let stored = self.implementations.get(&kind).map(String::as_str);
            if stored.is_some() && stored != analyzers.implementation(kind) {
                self.clear(kind);
                self.errors.remove(&kind);
                self.implementations.remove(&kind);
            }
        }
    }

    /// Drop the (large) similarity vector, e.g. before sending a list to the UI.
    pub fn without_embedding(mut self) -> Self {
        self.similarity = None;
        self
    }
}

/// What `CullerService::analyze` should do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    /// Exact photos to analyse. Takes precedence over `filter`.
    #[serde(default)]
    pub photo_ids: Option<Vec<PhotoId>>,
    /// Photos to analyse by filter. `None` (and no `photo_ids`) = every photo in the session,
    /// whatever its decision.
    #[serde(default)]
    pub filter: Option<crate::model::Filter>,
    /// Kinds to compute. Empty = every kind available on this platform.
    #[serde(default)]
    pub kinds: Vec<AnalysisKind>,
    /// Recompute even results that are stored and up to date.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeReport {
    /// Photos the request covered.
    pub photos: u64,
    /// Photos that needed (and got) at least one new result.
    pub analyzed: u64,
    /// Photos whose requested results were all stored and current.
    pub up_to_date: u64,
    /// Individual (photo, kind) results that failed; stored as errors.
    pub kind_errors: u64,
    /// Photos whose image couldn't be loaded at all (no preview and source unplugged).
    pub failed: Vec<(PhotoId, String)>,
}

/// The registry: which implementation serves each feature (`None` = not available here).
///
/// Fields are public so a platform can replace any single slot:
/// `analyzers.faces = Some(Arc::new(MyFaceDetector))`.
#[derive(Clone, Default)]
pub struct Analyzers {
    pub blur: Option<Arc<dyn BlurAnalyzer>>,
    pub exposure: Option<Arc<dyn ExposureAnalyzer>>,
    pub contrast: Option<Arc<dyn ContrastAnalyzer>>,
    pub noise: Option<Arc<dyn NoiseAnalyzer>>,
    pub color_cast: Option<Arc<dyn ColorCastAnalyzer>>,
    pub accidental: Option<Arc<dyn AccidentalAnalyzer>>,
    pub perceptual_hash: Option<Arc<dyn PerceptualHasher>>,
    pub similarity: Option<Arc<dyn SimilarityEmbedder>>,
    pub faces: Option<Arc<dyn FaceDetector>>,
    pub eyes: Option<Arc<dyn EyeStateAnalyzer>>,
    pub face_framing: Option<Arc<dyn FramingAnalyzer>>,
    pub face_quality: Option<Arc<dyn FaceQualityScorer>>,
    pub saliency: Option<Arc<dyn SaliencyDetector>>,
    pub subject_focus: Option<Arc<dyn SubjectFocusAnalyzer>>,
    pub aesthetics: Option<Arc<dyn AestheticsScorer>>,
    pub horizon: Option<Arc<dyn HorizonDetector>>,
}

/// What the registry can do, for the UI / debugging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerInfo {
    pub kind: AnalysisKind,
    /// `None` when no implementation is installed on this platform.
    pub implementation: Option<String>,
    pub inputs: Vec<AnalysisKind>,
}

impl Analyzers {
    /// Every pure-Rust implementation. Works on every platform.
    pub fn rust() -> Self {
        Self {
            blur: Some(Arc::new(blur::RustBlur)),
            exposure: Some(Arc::new(exposure::RustExposure)),
            contrast: Some(Arc::new(contrast::RustContrast)),
            noise: Some(Arc::new(noise::RustNoise)),
            color_cast: Some(Arc::new(color::RustColorCast)),
            accidental: Some(Arc::new(accidental::RustAccidental)),
            perceptual_hash: Some(Arc::new(hash::DHash)),
            similarity: Some(Arc::new(similarity::ColorLayout)),
            faces: None,
            eyes: Some(Arc::new(eyes::AspectRatioEyes)),
            face_framing: Some(Arc::new(framing::EdgeFraming)),
            face_quality: None,
            saliency: None,
            subject_focus: Some(Arc::new(subject_focus::LaplacianSubjectFocus)),
            aesthetics: None,
            horizon: None,
        }
    }

    /// Id of the implementation serving `kind`, if any.
    pub fn implementation(&self, kind: AnalysisKind) -> Option<&'static str> {
        use AnalysisKind as K;
        match kind {
            K::Blur => self.blur.as_ref().map(|a| a.implementation()),
            K::Exposure => self.exposure.as_ref().map(|a| a.implementation()),
            K::Contrast => self.contrast.as_ref().map(|a| a.implementation()),
            K::Noise => self.noise.as_ref().map(|a| a.implementation()),
            K::ColorCast => self.color_cast.as_ref().map(|a| a.implementation()),
            K::Accidental => self.accidental.as_ref().map(|a| a.implementation()),
            K::PerceptualHash => self.perceptual_hash.as_ref().map(|a| a.implementation()),
            K::Similarity => self.similarity.as_ref().map(|a| a.implementation()),
            K::Faces => self.faces.as_ref().map(|a| a.implementation()),
            K::Eyes => self.eyes.as_ref().filter(|_| self.faces.is_some()).map(|a| a.implementation()),
            K::FaceFraming => self.face_framing.as_ref().filter(|_| self.faces.is_some()).map(|a| a.implementation()),
            K::FaceQuality => self.face_quality.as_ref().map(|a| a.implementation()),
            K::Saliency => self.saliency.as_ref().map(|a| a.implementation()),
            K::SubjectFocus => self.subject_focus.as_ref().map(|a| a.implementation()),
            K::Aesthetics => self.aesthetics.as_ref().map(|a| a.implementation()),
            K::Horizon => self.horizon.as_ref().map(|a| a.implementation()),
        }
    }

    /// Kinds that can run here (face-based kinds need a face detector).
    pub fn available(&self) -> Vec<AnalysisKind> {
        AnalysisKind::ALL.into_iter().filter(|k| self.implementation(*k).is_some()).collect()
    }

    pub fn info(&self) -> Vec<AnalyzerInfo> {
        AnalysisKind::ALL
            .into_iter()
            .map(|kind| AnalyzerInfo { kind, implementation: self.implementation(kind).map(String::from), inputs: kind.inputs().to_vec() })
            .collect()
    }

    /// Run `kinds` on one image. Inputs a kind needs (faces for eyes, …) come from `prior`
    /// when present there, otherwise they are computed too and included in the result.
    /// Kinds without an implementation are skipped silently; failures land in `errors`.
    pub fn run(&self, photo_id: PhotoId, img: &AnalysisImage, kinds: &[AnalysisKind], prior: Option<&PhotoAnalysis>) -> PhotoAnalysis {
        use AnalysisKind as K;
        let mut want: Vec<AnalysisKind> = kinds.to_vec();
        for k in kinds {
            for dep in k.inputs() {
                if !prior.is_some_and(|p| p.has(*dep)) {
                    want.push(*dep);
                }
            }
        }
        let mut out = PhotoAnalysis::new(photo_id);

        for kind in AnalysisKind::ALL {
            if !want.contains(&kind) {
                continue;
            }
            let Some(implementation) = self.implementation(kind) else { continue };
            // Prefer inputs computed just now, then stored ones.
            let faces = out.faces.clone().or_else(|| prior.and_then(|p| p.faces.clone()));
            let saliency = out.saliency.clone().or_else(|| prior.and_then(|p| p.saliency.clone()));

            let result: crate::Result<serde_json::Value> = (|| {
                Ok(match kind {
                    K::Blur => json(&self.blur.as_ref().unwrap().blur(img)?),
                    K::Exposure => json(&self.exposure.as_ref().unwrap().exposure(img)?),
                    K::Contrast => json(&self.contrast.as_ref().unwrap().contrast(img)?),
                    K::Noise => json(&self.noise.as_ref().unwrap().noise(img)?),
                    K::ColorCast => json(&self.color_cast.as_ref().unwrap().color_cast(img)?),
                    K::Accidental => json(&self.accidental.as_ref().unwrap().accidental(img)?),
                    K::PerceptualHash => json(&self.perceptual_hash.as_ref().unwrap().hash(img)?),
                    K::Similarity => json(&self.similarity.as_ref().unwrap().embed(img)?),
                    K::Faces => json(&self.faces.as_ref().unwrap().faces(img)?),
                    K::Eyes => json(&self.eyes.as_ref().unwrap().eyes(img, faces.as_deref().unwrap_or_default())?),
                    K::FaceFraming => json(&self.face_framing.as_ref().unwrap().framing(faces.as_deref().unwrap_or_default())?),
                    K::FaceQuality => json(&self.face_quality.as_ref().unwrap().face_quality(img)?),
                    K::Saliency => json(&self.saliency.as_ref().unwrap().salient_regions(img)?),
                    K::SubjectFocus => {
                        let mut subjects: Vec<NormRect> = faces.iter().flatten().map(|f| f.bounds).collect();
                        if subjects.is_empty() {
                            subjects = saliency.iter().flatten().map(|s| s.bounds).collect();
                        }
                        json(&self.subject_focus.as_ref().unwrap().subject_focus(img, &subjects)?)
                    }
                    K::Aesthetics => json(&self.aesthetics.as_ref().unwrap().aesthetics(img)?),
                    K::Horizon => json(&self.horizon.as_ref().unwrap().horizon(img)?),
                })
            })();

            out.implementations.insert(kind, implementation.to_string());
            match result.and_then(|v| out.set_result_json(kind, v).map_err(|e| crate::CoreError::Other(e.to_string()))) {
                Ok(()) => {}
                Err(e) => {
                    out.errors.insert(kind, e.to_string());
                }
            }
        }
        out
    }
}

fn json<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{DynamicImage, ImageBuffer, Rgb};

    struct FakeFaces;
    impl FaceDetector for FakeFaces {
        fn implementation(&self) -> &'static str {
            "test.faces"
        }
        fn faces(&self, _: &AnalysisImage) -> crate::Result<Vec<Face>> {
            Ok(vec![Face {
                bounds: NormRect::new(0.0, 0.3, 0.2, 0.3),
                confidence: 0.9,
                roll: None,
                yaw: None,
                pitch: None,
                left_eye: vec![],
                right_eye: vec![],
            }])
        }
    }

    fn photo() -> AnalysisImage {
        AnalysisImage::from_image(DynamicImage::ImageRgb8(ImageBuffer::from_fn(320, 240, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 100]))))
    }

    #[test]
    fn rust_registry_runs_everything_available() {
        let a = Analyzers::rust();
        let r = a.run(7, &photo(), &AnalysisKind::ALL, None);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for k in a.available() {
            assert!(r.has(k), "{k:?} missing");
        }
        assert!(r.faces.is_none() && r.eyes.is_none(), "no face detector installed");
        assert_eq!(r.subject_focus.as_ref().unwrap().verdict, FocusVerdict::NoSubject);
        assert_eq!(r.implementations[&AnalysisKind::Blur], "rust.blur.v2");
    }

    #[test]
    fn dependencies_computed_or_taken_from_prior() {
        let mut a = Analyzers::rust();
        a.faces = Some(Arc::new(FakeFaces));
        // Asking only for framing computes faces too.
        let r = a.run(1, &photo(), &[AnalysisKind::FaceFraming], None);
        assert_eq!(r.faces.as_ref().unwrap().len(), 1);
        assert_eq!(r.face_framing.as_ref().unwrap().cut_faces, vec![0]);
        // With faces already stored, only framing is produced.
        let r2 = a.run(1, &photo(), &[AnalysisKind::FaceFraming], Some(&r));
        assert!(r2.faces.is_none() && r2.face_framing.is_some());
    }

    #[test]
    fn json_roundtrip_and_merge() {
        let r = Analyzers::rust().run(3, &photo(), &AnalysisKind::ALL, None);
        let mut copy = PhotoAnalysis::new(3);
        copy.merge(r.clone());
        assert_eq!(copy, r);
        let back: PhotoAnalysis = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
