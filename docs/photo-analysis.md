# Photo analysis

Static, on-device analysis of every reason someone might reject a photo — blur, bad exposure,
closed eyes, cut-off faces, near-duplicates and more. No cloud, no model downloads.

This doc is the map: **what exists, where it lives, how to call it, how to swap an engine.**

## The design in one paragraph

Every feature (a "kind") is its own module in `crates/cull-core/src/analysis/`, with **one
trait** (the stable interface), **one result type**, and zero or more implementations
("engines"). Callers only ever see the trait and the result type, so an engine can be replaced
— pure Rust ↔ Apple Vision ↔ an ONNX model later — without touching anything else. The
`Analyzers` registry holds one optional engine per kind. `Analyzers::rust()` fills in the pure-Rust
ones (every platform); `cull_platform_apple::vision::install()` adds Apple Vision on macOS (iOS
later). Results are stored per photo and kind in SQLite along with the engine id that made them.
When the engine id changes, the stored result counts as stale and is recomputed.

```
cached master preview (2048 px JPEG, upright)        ← analysis never reads the originals
        │
        ▼
  AnalysisImage  (lazy, shared decodes: full RGB/grey ≤2048 px, working RGB/grey ≤640 px, JPEG bytes)
        │
        ▼
  pipeline::Pipeline (analysis/pipeline.rs)           ← list of independent PhotoSteps (blur, …, `subjects` = faces→eyes/framing,
        │                                                saliency→subject focus). Photos run in parallel, a photo's steps in order.
        ▼                                                Each step calls Analyzers::run for its kinds.
  StepResult per kind  ──save──▶  SQLite `analysis` table (photo_id, kind, implementation, result JSON, error)
        │                          A result counts only while its implementation id matches the registered step
        ▼                          (PhotoAnalysis::retain_current); rows of removed/replaced engines stay, ignored.
  PhotoAnalysis
        │
        ├─▶ issues::issues()            → reject reasons ("eyes closed", "blurry") for problem tabs
        ├─▶ grouping::similar_groups()  → near-duplicate rows, best first
        └─▶ ranking::quality_score()    → "best of the group" number
```

## File map

| Path | What |
|---|---|
| `crates/cull-core/src/analysis/mod.rs` | `AnalysisKind`, `PhotoAnalysis`, `Analyzers` registry + `run`, `AnalyzeRequest`/`AnalyzeReport` |
| `crates/cull-core/src/analysis/image.rs` | `AnalysisImage`, the single input type (lazy decodes) |
| `crates/cull-core/src/analysis/geometry.rs` | `NormRect`, `NormPoint` (0–1, **top-left origin**) |
| `crates/cull-core/src/analysis/<feature>.rs` | One file per feature (table below); `blur.rs` also holds the shared Laplacian helpers (`laplacian_variance`, `squash`) |
| `crates/cull-core/src/analysis/issues.rs` | Results → `Issue` list (the problem-tab layer) |
| `crates/cull-core/src/analysis/grouping.rs` | Near-duplicate grouping (union-find) |
| `crates/cull-core/src/analysis/ranking.rs` | `quality_score` for best-of-group |
| `crates/cull-core/src/api.rs` | `CullerService` analysis methods (section "photo analysis") |
| `crates/cull-core/src/store.rs` | `analysis` table, `save_analysis`, `load_analyses`, `analysis_index` |
| `crates/cull-platform-apple/src/vision.rs` | Apple Vision engines + `install()` / `analyzers()` |
| `crates/cull-platform-apple/examples/analyze.rs` | CLI: run everything on a folder, timings, calibration |
| `crates/cull-platform-fs/tests/analysis.rs` | End-to-end: scan → analyze (card unplugged) → issues → groups |
| `src-tauri/src/lib.rs` | Tauri commands (section "photo analysis"); installs Vision on macOS |
| `ui/src/lib/api/analysisTypes.ts` | TypeScript mirrors of every result type |
| `ui/src/lib/api/CullerApi.ts` / `tauriApi.ts` / `mockAnalysis.ts` | UI API + real and mock backends |

## Features

"Inputs" = other kinds whose results this one consumes; the registry computes them first (or
reuses stored ones). Engine ids are what `implementation()` returns and what's stored in the DB.

| Kind (`AnalysisKind`) | Trait → method | Result | Engines (id) | Inputs | Catches |
|---|---|---|---|---|---|
| `blur` | `BlurAnalyzer::blur` | `Blur { score, peak, directionality, verdict: sharp\|soft\|motionBlur }` | `rust.blur.v1` | – | Missed focus, shake, motion blur. `peak` = sharpest 10% of tiles, so sharp-subject/soft-background isn't flagged |
| `exposure` | `ExposureAnalyzer::exposure` | `Exposure { mean, shadowsClipped, highlightsClipped, p05, p95, verdict }` | `rust.exposure.v1` | – | Too dark / blown out |
| `contrast` | `ContrastAnalyzer::contrast` | `Contrast { rms, range, flat }` | `rust.contrast.v1` | – | Flat, hazy shots |
| `noise` | `NoiseAnalyzer::noise` | `Noise { sigma, level: low\|medium\|high }` | `rust.noise.v1` (Immerkær, flattest 20% of blocks) | – | Grainy high-ISO shots |
| `colorCast` | `ColorCastAnalyzer::color_cast` | `ColorCast { warmCool, greenMagenta, strength, hue }` | `rust.color_cast.v2` (gray-world) | – | Bad white balance (hint only: sunsets lean on purpose) |
| `accidental` | `AccidentalAnalyzer::accidental` | `Accidental { likely, reason: dark\|blank }` | `rust.accidental.v1` | – | Pocket shots, lens cap, blank wall |
| `perceptualHash` | `PerceptualHasher::hash` | `PerceptualHash` (64-bit, JSON = 16 hex chars) | `rust.dhash.v1` | – | Near-duplicate grouping (fast, weak on smooth scenes) |
| `similarity` | `SimilarityEmbedder::embed` | `Embedding { model, vector }` | `apple.vision.feature_print.r2` (macOS), `rust.color_layout.v1` (fallback) | – | Near-duplicate grouping that survives reframing |
| `faces` | `FaceDetector::faces` | `Vec<Face { bounds, confidence, roll, yaw, pitch, leftEye, rightEye }>` | `apple.vision.face_landmarks.v1` | – | Input for eyes/framing/focus |
| `eyes` | `EyeStateAnalyzer::eyes` | `Vec<EyeState { face, leftOpenness, rightOpenness, closed }>` | `rust.eye_aspect_ratio.v1` | faces | **Closed eyes** (eye height ÷ width < 0.16) |
| `faceFraming` | `FramingAnalyzer::framing` | `FaceFraming { cutFaces }` | `rust.face_framing.v1` | faces | Faces cut off by the frame edge |
| `faceQuality` | `FaceQualityScorer::face_quality` | `Vec<FaceQuality { bounds, quality 0–1 }>` | `apple.vision.face_capture_quality.v1` | – | Best face in a burst (relative, not absolute) |
| `saliency` | `SaliencyDetector::salient_regions` | `Vec<SalientRegion { bounds, confidence }>` | `apple.vision.attention_saliency.v1` | – | Subject regions for photos without faces |
| `subjectFocus` | `SubjectFocusAnalyzer::subject_focus` | `SubjectFocus { subject, frame, verdict }` | `rust.subject_focus.v2` | faces, saliency | Subject soft / focus landed on the background |
| `aesthetics` | `AestheticsScorer::aesthetics` | `Aesthetics { overall ≈ −1…1, isUtility }` | `apple.vision.aesthetics.v1` (macOS 15+/iOS 18+) | – | Unappealing shots; screenshots/documents |
| `horizon` | `HorizonDetector::horizon` | `Horizon { detected, angleDegrees, confidence }` | `apple.vision.horizon.v1` | – | Tilted horizons (2°–12°) |

Without Apple Vision (Rust only), `faces`, `eyes`, `faceFraming`, `faceQuality`, `saliency`,
`aesthetics` and `horizon` are unavailable (`eyes`/`faceFraming` need a face engine). `subjectFocus` still runs but
reports `noSubject`.

Thresholds are `pub const`s next to each feature (e.g. `blur::SOFT_PEAK`, `eyes::CLOSED_OPENNESS`,
`color::CAST_STRENGTH`, `similarity::near_duplicate_distance`). If you change how a result is
computed, **bump the engine id** (`v1` → `v2`) so stored results get recomputed.

### Issues (`issues.rs`)

`issues(&PhotoAnalysis) -> Vec<Issue { kind, severity 0–1, detail }>`, most severe first.
`IssueKind`: `blurry`, `motionBlur`, `subjectOutOfFocus`, `underexposed`, `overexposed`, `flat`,
`noisy`, `colorCast`, `eyesClosed`, `faceCutOff`, `poorFaceQuality`, `tilted`, `lowAesthetic`,
`utility`, `accidental`. This is the layer for "problem tabs" (Blurry, Eyes closed, …).

### Near-duplicate groups (`grouping.rs`)

Exact duplicates (same bytes) already collapse into one photo through the content fingerprint.
`similar_groups` groups photos that **look** the same. A pair is linked when it was taken within
`maxGapSecs` (default 600, `null` = any time) **and**:

- with Vision feature prints: embedding distance ≤ 0.45. The embedding alone decides.
- with Rust colour-layout embeddings (weak): embedding ≤ 0.35 **and** dHash ≤ 10 bits.
- with hashes only: dHash ≤ `maxHashBits` (10).

Links merge transitively (union-find) into groups of 2+. Each group lists `photoIds` best-first
by `ranking::quality_score` (sharpness, subject focus, face quality, aesthetics, minus issues),
with `scores` in the same order and `best` = the first. Options: `GroupingOptions { maxGapSecs,
maxHashBits, useEmbeddings, embeddingTolerance (×threshold), window }`.

dHash alone is unreliable on smooth, low-detail photos (sky, water, macro). Their hashes mostly
encode vignetting, so unrelated photos collide. That's why an embedding always takes priority.

## How to call it

### UI (TypeScript, `api` from `ui/src/lib/api`)

```ts
await api.analyzerInfo();                              // which engine serves each kind here
await api.analyzePhotos();                             // all photos in the session, missing/stale only
await api.analyzePhotos({ filter, kinds: ['blur', 'eyes'], force: true });
await api.analyzePhotos({ photoIds: [12, 13] });
api.onEvent(e => e.type === 'analysisProgress' && …);  // { done, total } per batch of 16
await api.photoIssues(filter);                         // [{ photoId, issues: [{ kind, severity, detail }] }]
await api.similarGroups(filter, { maxGapSecs: null }); // [{ photoIds (best first), scores, best }]
await api.photoAnalyses(filter);                       // full results (no embeddings), queue order
await api.photoAnalysis(id);                           // one photo, including its embedding
```

`filter` is the same `Filter` the queue uses (day range + decision status). `analyzePhotos`
with no filter covers every photo in the session, whatever its decision. The mock backend
(`npm run dev:mock`) returns deterministic fake results with the same shapes.

### Tauri commands (`src-tauri/src/lib.rs`)

`analyzer_info`, `analyze_photos { request }`, `photo_analysis { id }`,
`photo_analyses { filter }`, `photo_issues { filter }`, `similar_groups { filter, options? }`.
All are async (`spawn_blocking`) except `analyzer_info`.

### Rust (`CullerService`)

```rust
svc.set_analyzers(cull_platform_apple::vision::analyzers());   // or Analyzers::rust() (default)
let report = svc.analyze(&AnalyzeRequest::default(), &sink)?;  // blocking, CPU-heavy
let issues = svc.photo_issues(&filter)?;
let groups = svc.similar_groups(&filter, &GroupingOptions::default())?;
let one = svc.photo_analysis(id)?;
```

Lower level, with no service or DB (for tools and tests):

```rust
let img = AnalysisImage::from_jpeg(master_jpeg_bytes);         // or ::from_image(DynamicImage)
let result = analyzers.run(photo_id, &img, &[AnalysisKind::Eyes], None);   // computes faces too
let blur = RustBlur.blur(&img)?;                                // any single engine directly
```

### When analysis runs

Analysis runs only when something calls `analyze` / `analyzePhotos`. Scanning doesn't trigger it.
That's a UX decision still to make: right after a scan in the background, per day when opened, or
on demand. `analyze` is incremental, so calling it often is cheap once results exist. It runs 4
photos in parallel (`ANALYZE_THREADS`) and saves every 16 (`ANALYZE_BATCH`).

## Swapping or adding an engine

1. Implement the feature's trait (e.g. `impl FaceDetector for OnnxFaces`) anywhere: core for
   pure Rust, a platform crate for native APIs. Give it a new `implementation()` id.
2. Put it in the registry slot: `analyzers.faces = Some(Arc::new(OnnxFaces));` then
   `svc.set_analyzers(analyzers)`.
3. Done. Stored results from the old engine are now stale (different id) and recompute on
   the next `analyze`. Result types, issues, grouping, the UI: all unchanged.

A **new step** (a new kind of analysis the pipeline runs) is one `steps.push(...)` line in
`Pipeline::from_analyzers` (`analysis/pipeline.rs`). Steps never read each other's output; analyzers that
depend on each other (faces → eyes) go inside one step. `analyze()` passes no source folder, so a photo
whose cached preview is missing is reported in `failed` and never read from the card.

A **new feature** means: a module with result type + trait (+ engine), a variant in `AnalysisKind`
(+ `ALL`, `as_str`, `inputs`), a field in `PhotoAnalysis` and `Analyzers` (+ the `per_kind!` list,
`implementation()`, `run()`), optionally an `IssueKind`, then the TS mirror in `analysisTypes.ts`
and `mockAnalysis.ts`.

Coordinates: everything is normalised 0–1 with a **top-left** origin on the upright master.
Vision uses bottom-left; `vision.rs` converts (`rect()`, `region_points()`).

## Performance (measured)

On Apple silicon, 117 test photos (3000×2000 JPEGs), one thread, app-equivalent input (2048 px
master JPEG), via `cargo run -p cull-platform-apple --example analyze -- test-photos`:

| | ms / photo |
|---|---|
| blur (includes the shared decode, ~60 ms) | ~78 |
| noise | ~39 |
| perceptualHash | ~21 |
| each Vision request (faces, quality, saliency, aesthetics, horizon, feature print) | ~8–11 |
| everything else | < 12 each |
| **total, all 16 kinds** | **~240** |

With 4 threads that's about 1,000 photos in ~1 minute. Each Vision engine decodes the JPEG
itself. A shared `VNImageRequestHandler` would save some of that if it ever matters.

## Calibration and tools

```sh
cargo run -p cull-platform-apple --example analyze -- <dir>               # Rust + Vision, per-photo line + timings + groups
cargo run -p cull-platform-apple --example analyze -- <dir> --rust-only   # what non-Apple platforms get
cargo run -p cull-platform-apple --example analyze -- <dir> --calibrate   # near-dup vs unrelated distance percentiles per embedding
cargo run -p cull-platform-apple --example analyze -- <dir> --pairs       # 15 closest pairs (embedding, hash bits)
cargo run -p cull-platform-apple --example analyze -- <dir> --json        # one PhotoAnalysis JSON per line
```

Calibration on `test-photos` (3% crop + brightened copies vs 117 unrelated photos):

| Model | Near-dups, max distance | Unrelated, min distance | Threshold |
|---|---|---|---|
| Vision feature print r2 | 0.29 | 0.59 | 0.45 |
| Rust colour layout | 0.27 | 0.43 | 0.35 (+ hash must agree) |

Real 8% crops of 5 photos all grouped correctly in both modes, and the 117 unrelated photos
formed no groups.

## Known limits / not yet verified

- **Eyes and faces are untested on real faces.** `test-photos` (Lorem Picsum) has almost
  no people, so `CLOSED_OPENNESS = 0.16` is from the literature, not tuned. Check it with real
  portraits/group shots before trusting the Eyes-closed tab.
- Noise thresholds weren't calibrated on real high-ISO files (stock photos are clean).
  `sigma` is measured on the 2048 px preview, so it reads lower than full-resolution tools.
- Vision's horizon request always reports confidence 1.0, so the confidence gate is a no-op
  with Vision. Tilts of 2°–12° are flagged; treat them as hints.
- Colour cast and aesthetics are hints, never rejects on their own (graded/sunset shots lean on
  purpose; Vision's `isUtility` flagged ~12% of the stock set).
- RAW files aren't analysed (they aren't scanned yet either). Once RAW previews exist, they get
  analysed through the same master-preview path.
- Not tried in the running Tauri app yet: the commands are wired and the core/Vision paths are
  tested via `cargo test` and the example, but no UI calls them.
