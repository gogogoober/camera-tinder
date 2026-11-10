//! Apple Vision implementations of the `cull_core::analysis` traits (macOS today, iOS later —
//! the same Vision API exists there).
//!
//! Each struct implements exactly one core trait and knows nothing about the others, so any
//! one of them can be swapped for a different engine without touching callers. All of them
//! hand Vision the upright master-preview JPEG (`AnalysisImage::jpeg`) and convert Vision's
//! bottom-left-origin coordinates to the core's top-left convention.
//!
//! Use [`install`] to add every implementation this OS supports to an `Analyzers` registry.

use std::sync::Arc;

use cull_core::analysis::{
    Aesthetics, AestheticsScorer, AnalysisImage, Analyzers, Embedding, Face, FaceDetector, FaceQuality, FaceQualityScorer, Horizon, HorizonDetector, NormPoint, NormRect,
    SalientRegion, SaliencyDetector, SimilarityEmbedder,
};
use cull_core::{CoreError, Result};
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyClass;
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber};
use objc2_vision::{
    VNCalculateImageAestheticsScoresRequest, VNDetectFaceCaptureQualityRequest, VNDetectFaceLandmarksRequest, VNDetectHorizonRequest, VNElementType, VNFaceLandmarkRegion2D,
    VNGenerateAttentionBasedSaliencyImageRequest, VNGenerateImageFeaturePrintRequest, VNImageRequestHandler, VNRequest,
};

/// Run one Vision request on the image. Results are read from the request afterwards.
fn perform(img: &AnalysisImage, request: &VNRequest) -> Result<()> {
    let data = NSData::with_bytes(img.jpeg()?);
    let handler = VNImageRequestHandler::initWithData_options(VNImageRequestHandler::alloc(), &data, &NSDictionary::new());
    let requests = NSArray::from_slice(&[request]);
    handler.performRequests_error(&requests).map_err(|e| CoreError::Other(format!("Vision: {}", e.localizedDescription())))
}

/// True when this OS ships the given Vision class (newer requests need newer macOS/iOS).
fn has_class(name: &std::ffi::CStr) -> bool {
    AnyClass::get(name).is_some()
}

/// Vision rect (normalised, bottom-left origin) → core rect (top-left origin).
fn rect(r: CGRect) -> NormRect {
    NormRect::new(r.origin.x as f32, (1.0 - r.origin.y - r.size.height) as f32, r.size.width as f32, r.size.height as f32)
}

fn degrees(n: Option<Retained<NSNumber>>) -> Option<f32> {
    n.map(|n| n.doubleValue().to_degrees() as f32)
}

/// Landmark points are normalised to the face box (bottom-left origin); map them into the image.
fn region_points(region: Option<Retained<VNFaceLandmarkRegion2D>>, face: CGRect) -> Vec<NormPoint> {
    let Some(region) = region else { return vec![] };
    let n = unsafe { region.pointCount() };
    let ptr = unsafe { region.normalizedPoints() };
    if ptr.is_null() || n == 0 {
        return vec![];
    }
    let pts = unsafe { std::slice::from_raw_parts(ptr, n) };
    pts.iter()
        .map(|p| NormPoint {
            x: (face.origin.x + p.x * face.size.width) as f32,
            y: (1.0 - (face.origin.y + p.y * face.size.height)) as f32,
        })
        .collect()
}

/// Faces with eye outlines: `VNDetectFaceLandmarksRequest`.
pub struct VisionFaces;

impl FaceDetector for VisionFaces {
    fn implementation(&self) -> &'static str {
        "apple.vision.face_landmarks.v1"
    }

    fn faces(&self, img: &AnalysisImage) -> Result<Vec<Face>> {
        autoreleasepool(|_| {
            let req = unsafe { VNDetectFaceLandmarksRequest::new() };
            perform(img, &req)?;
            let Some(results) = (unsafe { req.results() }) else { return Ok(vec![]) };
            Ok(results
                .iter()
                .map(|f| {
                    let bb = unsafe { f.boundingBox() };
                    let landmarks = unsafe { f.landmarks() };
                    let (left_eye, right_eye) = match &landmarks {
                        Some(l) => (region_points(unsafe { l.leftEye() }, bb), region_points(unsafe { l.rightEye() }, bb)),
                        None => (vec![], vec![]),
                    };
                    Face {
                        bounds: rect(bb),
                        confidence: unsafe { f.confidence() },
                        roll: degrees(unsafe { f.roll() }),
                        yaw: degrees(unsafe { f.yaw() }),
                        pitch: degrees(unsafe { f.pitch() }),
                        left_eye,
                        right_eye,
                    }
                })
                .collect())
        })
    }
}

/// Per-face capture quality: `VNDetectFaceCaptureQualityRequest`.
pub struct VisionFaceQuality;

impl FaceQualityScorer for VisionFaceQuality {
    fn implementation(&self) -> &'static str {
        "apple.vision.face_capture_quality.v1"
    }

    fn face_quality(&self, img: &AnalysisImage) -> Result<Vec<FaceQuality>> {
        autoreleasepool(|_| {
            let req = unsafe { VNDetectFaceCaptureQualityRequest::new() };
            perform(img, &req)?;
            let Some(results) = (unsafe { req.results() }) else { return Ok(vec![]) };
            Ok(results
                .iter()
                .filter_map(|f| {
                    let q = unsafe { f.faceCaptureQuality() }?;
                    Some(FaceQuality { bounds: rect(unsafe { f.boundingBox() }), quality: q.doubleValue() as f32 })
                })
                .collect())
        })
    }
}

/// Where attention goes: `VNGenerateAttentionBasedSaliencyImageRequest`.
pub struct VisionSaliency;

impl SaliencyDetector for VisionSaliency {
    fn implementation(&self) -> &'static str {
        "apple.vision.attention_saliency.v1"
    }

    fn salient_regions(&self, img: &AnalysisImage) -> Result<Vec<SalientRegion>> {
        autoreleasepool(|_| {
            let req = unsafe { VNGenerateAttentionBasedSaliencyImageRequest::new() };
            perform(img, &req)?;
            let Some(results) = (unsafe { req.results() }) else { return Ok(vec![]) };
            let mut out = Vec::new();
            for obs in results.iter() {
                if let Some(objects) = unsafe { obs.salientObjects() } {
                    for o in objects.iter() {
                        out.push(SalientRegion { bounds: rect(unsafe { o.boundingBox() }), confidence: unsafe { o.confidence() } });
                    }
                }
            }
            Ok(out)
        })
    }
}

/// Overall aesthetics + utility flag: `VNCalculateImageAestheticsScoresRequest` (macOS 15+/iOS 18+).
pub struct VisionAesthetics;

impl VisionAesthetics {
    pub fn is_supported() -> bool {
        has_class(c"VNCalculateImageAestheticsScoresRequest")
    }
}

impl AestheticsScorer for VisionAesthetics {
    fn implementation(&self) -> &'static str {
        "apple.vision.aesthetics.v1"
    }

    fn aesthetics(&self, img: &AnalysisImage) -> Result<Aesthetics> {
        if !Self::is_supported() {
            return Err(CoreError::Unsupported("aesthetics scores need macOS 15 / iOS 18".into()));
        }
        autoreleasepool(|_| {
            let req = unsafe { VNCalculateImageAestheticsScoresRequest::new() };
            perform(img, &req)?;
            let obs = unsafe { req.results() }.and_then(|r| r.firstObject()).ok_or_else(|| CoreError::Other("Vision returned no aesthetics score".into()))?;
            Ok(Aesthetics { overall: unsafe { obs.overallScore() }, is_utility: unsafe { obs.isUtility() } })
        })
    }
}

/// Horizon angle: `VNDetectHorizonRequest`.
pub struct VisionHorizon;

impl HorizonDetector for VisionHorizon {
    fn implementation(&self) -> &'static str {
        "apple.vision.horizon.v1"
    }

    fn horizon(&self, img: &AnalysisImage) -> Result<Horizon> {
        autoreleasepool(|_| {
            let req = unsafe { VNDetectHorizonRequest::new() };
            perform(img, &req)?;
            Ok(match unsafe { req.results() }.and_then(|r| r.firstObject()) {
                Some(h) => Horizon { detected: true, angle_degrees: (unsafe { h.angle() }).to_degrees() as f32, confidence: unsafe { h.confidence() } },
                None => Horizon { detected: false, angle_degrees: 0.0, confidence: 0.0 },
            })
        })
    }
}

/// Image feature print for near-duplicate grouping: `VNGenerateImageFeaturePrintRequest`,
/// revision 2 (unit-length vectors; distance 0–2).
pub struct VisionFeaturePrint;

impl SimilarityEmbedder for VisionFeaturePrint {
    fn implementation(&self) -> &'static str {
        "apple.vision.feature_print.r2"
    }

    fn embed(&self, img: &AnalysisImage) -> Result<Embedding> {
        autoreleasepool(|_| {
            let req = unsafe { VNGenerateImageFeaturePrintRequest::new() };
            unsafe { req.setRevision(2) };
            perform(img, &req)?;
            let obs = unsafe { req.results() }.and_then(|r| r.firstObject()).ok_or_else(|| CoreError::Other("Vision returned no feature print".into()))?;
            let bytes = unsafe { obs.data() }.to_vec();
            let vector: Vec<f32> = match unsafe { obs.elementType() } {
                VNElementType::Float => bytes.as_chunks::<4>().0.iter().map(|c| f32::from_ne_bytes(*c)).collect(),
                VNElementType::Double => bytes.as_chunks::<8>().0.iter().map(|c| f64::from_ne_bytes(*c) as f32).collect(),
                other => return Err(CoreError::Other(format!("unexpected feature print element type {}", other.0))),
            };
            Ok(Embedding { model: self.implementation().into(), vector })
        })
    }
}

/// Add every Vision implementation this OS supports to `analyzers`, replacing the Rust
/// fallbacks where Vision is better (similarity). Rust implementations stay for the rest.
pub fn install(analyzers: &mut Analyzers) {
    analyzers.faces = Some(Arc::new(VisionFaces));
    analyzers.face_quality = Some(Arc::new(VisionFaceQuality));
    analyzers.saliency = Some(Arc::new(VisionSaliency));
    analyzers.horizon = Some(Arc::new(VisionHorizon));
    analyzers.similarity = Some(Arc::new(VisionFeaturePrint));
    if VisionAesthetics::is_supported() {
        analyzers.aesthetics = Some(Arc::new(VisionAesthetics));
    }
}

/// Rust analyzers plus everything Vision offers here.
pub fn analyzers() -> Analyzers {
    let mut a = Analyzers::rust();
    install(&mut a);
    a
}
