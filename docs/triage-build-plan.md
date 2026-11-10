# Smart triage: build plan (2026-11-10)

How to build [smart-triage.md](smart-triage.md) (the agreed UX; read it first, and
[photo-analysis.md](photo-analysis.md) for the analyzers). Written so each phase can be built on its own,
in order, by someone who has not seen the design conversation.

## Rules for whoever builds this

- **High value, low-to-medium complexity only.** Don't add priority queues, pause, caches, history tables,
  settings or abstraction layers that this plan does not list. The "Cut" list at the end says what was rejected
  and why. Speed is not the goal; being able to change and optimise later is.
- Follow `AGENTS.md`: all logic in `cull-core`; a feature = `CullerService` method → thin Tauri command →
  `CullerApi` + `tauriApi` + `mockApi`; Rust types crossing to the UI use `#[serde(rename_all = "camelCase")]`
  and are mirrored in `ui/src/lib/api/`.
- **Analysis reads only cached previews, never original files** (the card is read only at scan and export).
- One decision vocabulary everywhere: ✕ Nope (←, stored `dislike`), ⊘ Hate (↓), ★ Favorite (↑),
  ✓ Keep (→, stored `like`), Skip (space). Reuse `ui/src/lib/decisions.ts`; don't invent screen-only verbs.
- End every phase with `cargo test --workspace` and `npm run check` green, then commit that phase on its own.
- Don't change the scan's file-reading code or `source_safety.rs` rules except where a phase says so.

## Architecture

```
                 ┌──────────── cull-core ─────────────────────────────────────────────────────┐
 scan()  ──ScanSaved──▶ WakeOnSaved sink ──wake()──▶ AnalysisWorker (1 thread, owned by service)
 (4 threads,                                          │ analyze_pending(): one pass
  saves every 32)                                     │   photos in session, capture order
                                                      │   ask DB which (photo, step) are stale
                                                      │   chunks of ANALYZE_BATCH, ANALYZE_THREADS in parallel
                                                      │     per photo: cached 2048 master (no source!) → decode once
                                                      │       for each stale step: stop flag? → step.run()
                                                      │   save each chunk in one transaction
                                                      ▼
                                  analysis table: (photo_id, kind TEXT) → implementation, result JSON, error
                                                      │  read back only if implementation == current step's
 plan(filter) ─▶ photos + moments + bursts + triage ◀─┘  triage::build(): issues → group → rank → route
                                                         (pure functions, computed on every request)
 log_outcomes([...]) ─▶ outcomes table
                 └───────────────────────────────────────────────────────────────────────────────┘
 src-tauri: start worker after set_analyzers; commands stop_analysis / resume_analysis / log_outcomes
 ui: Filmstrip (burst stacks, red slivers), BurstStrip (new), Cull (navigation), Rescue (new screen), Home (Stop analysis)
```

### Files

```
crates/cull-core/src/
  analysis/
    pipeline.rs        NEW  PhotoStep trait, StepResult, Pipeline (list of steps), the 12 adapter steps,
                            stale_steps(), run_photo()
    mod.rs             EDIT pub mod pipeline; PhotoAnalysis::retain_current()
    grouping.rs        EDIT GroupingOptions.burst_gap_secs (default None)
  triage/
    mod.rs             NEW  types (TriageItem, Question, GroupKind…), PLAN_VERSION, build()
    route.rs           NEW  REJECT_KINDS, HINT_KINDS, SWEEP_MIN_SEVERITY, routing per photo
  worker.rs            NEW  AnalysisWorker: thread, wake condvar, stop flag
  api.rs               EDIT analyze() on the pipeline; analyze_pending(); worker start/stop/resume;
                            plan() adds triage; log_outcomes(); scan() wraps the sink
  model.rs             EDIT Plan gains `triage: Vec<TriageItem>` and `triage_version`
  store.rs             EDIT result_index(), save_results(), outcomes table + insert_outcomes()
  lib.rs               EDIT pub mod triage; mod worker
crates/cull-platform-fs/tests/
  source_safety.rs     EDIT analysis never opens a source
  analysis.rs          EDIT pipeline + worker tests (or a new tests/triage.rs)
src-tauri/src/lib.rs   EDIT start worker; stop_analysis, resume_analysis, log_outcomes commands
ui/src/lib/
  api/types.ts         EDIT Plan.triage, TriageItem, Outcome types
  api/CullerApi.ts     EDIT stopAnalysis, resumeAnalysis, logOutcomes
  api/tauriApi.ts      EDIT same
  api/mockApi.ts       EDIT same + mock triage in plan()
  api/mockTriage.ts    NEW  deterministic fake triage for the mock backend
  triage.ts            NEW  pure helpers: stops, groups, next/prev, set-aside list (unit-testable)
  components/
    Filmstrip.svelte   EDIT burst stacks, red slivers, hidden group members
    BurstStrip.svelte  NEW  the small frame timeline under the photo
    Cull.svelte        EDIT stops, frames, opened set-aside photo, keys, end → Rescue
    Rescue.svelte      NEW  the end-of-session grid
    Home.svelte        EDIT analysis progress + Stop/Resume analysis
  App.svelte           EDIT 'rescue' view
docs/                  EDIT photo-analysis.md (pipeline section), AGENTS.md (architecture lines, roadmap)
```

---

## P1 — Pipeline core (no behaviour change)

Goal: analysis becomes a list of independent steps. Adding a step = one new type + one line in
`Pipeline::from_analyzers`. Existing commands, tests and UI keep working.

### 1.1 `analysis/pipeline.rs`

```rust
/// One result a step produced for one photo. `kind` is the stored key (`analysis.kind`), e.g. "blur".
pub struct StepResult {
    pub kind: String,
    pub implementation: String,
    pub result: std::result::Result<serde_json::Value, String>,
}

/// A unit of per-photo analysis. Steps never read each other's output: analyzers that depend on
/// each other live inside one step and pass results along in code.
pub trait PhotoStep: Send + Sync {
    /// Stable id, for logs and errors ("blur", "subjects").
    fn id(&self) -> &'static str;
    /// Every result this step writes, with the implementation id it writes it with.
    /// A photo needs this step when any of these is missing or stored with another implementation.
    fn outputs(&self) -> Vec<(String, String)>;
    /// Compute. Must return one StepResult per output (errors included). Never panics on bad input.
    fn run(&self, photo_id: PhotoId, img: &AnalysisImage) -> Vec<StepResult>;
}

pub struct Pipeline { pub steps: Vec<Arc<dyn PhotoStep>> }
```

**Adapter steps** (built from today's `Analyzers`, so engines and `Analyzers::run` stay as they are):

- `KindStep { analyzers: Arc<Analyzers>, kind: AnalysisKind }`: `id = kind.as_str()`;
  `outputs = [(kind.as_str(), analyzers.implementation(kind))]`; `run` calls
  `analyzers.run(photo_id, img, &[kind], None)` and converts the returned `PhotoAnalysis` into `StepResult`s
  (helper `fn to_results(a: &PhotoAnalysis) -> Vec<StepResult>` using `result_json`, `errors`, `implementations`).
- `SubjectsStep { analyzers }` for the dependent cluster: kinds `Faces, Eyes, FaceFraming, Saliency, SubjectFocus`.
  `outputs` = those of the five with an implementation (`analyzers.implementation(k).is_some()`; eyes and framing
  already report `None` without a face detector). `run` = `analyzers.run(photo_id, img, &those_kinds, None)`,
  which already runs faces before eyes/framing and saliency before subject focus.
- `Pipeline::from_analyzers(a: Arc<Analyzers>)`: one `KindStep` for each of `Blur, Exposure, Contrast, Noise,
  ColorCast, Accidental, PerceptualHash, Similarity, FaceQuality, Aesthetics, Horizon` that has an implementation,
  plus `SubjectsStep` if any of its kinds has one. On macOS with Vision that's 12 steps.
  **This function is the registry: a new step is one `steps.push(...)` line here.**

Pure helpers (unit-tested, no I/O):

```rust
/// Indexes of steps that `stored` (kind → implementation, for one photo) doesn't satisfy.
pub fn stale_steps(pipeline: &Pipeline, stored: Option<&HashMap<String, String>>, force: bool) -> Vec<usize>;
/// Run the given steps in order; before each, return early if `stop` is set.
/// Returns the results so far and whether it stopped.
pub fn run_photo(pipeline: &Pipeline, steps: &[usize], photo_id: PhotoId, img: &AnalysisImage, stop: &AtomicBool) -> (Vec<StepResult>, bool);
```

Steps of one photo run one after another. They are independent, so running them in parallel later is a change
inside `run_photo` only (don't do it now).

### 1.2 Store (`store.rs`)

- `result_index(ids) -> HashMap<PhotoId, HashMap<String, String>>`: like `analysis_index` but keeps every `kind`
  string (unknown kinds included). Chunk by 500 like the existing queries.
- `save_results(photo_id, &[StepResult])`: same upsert as `save_analysis`
  (`ON CONFLICT(photo_id, kind) DO UPDATE`), `result` = JSON text or NULL, `error` = message or NULL.
  Reimplement `save_analysis` on top of it (it converts a `PhotoAnalysis` with `to_results`).
- No schema change. `analysis.kind` is already TEXT.

### 1.3 Read only current results

`PhotoAnalysis::retain_current(&mut self, analyzers: &Analyzers)`: for every kind whose stored implementation
(`self.implementations[kind]`) differs from `analyzers.implementation(kind)` (or that has none now), clear the
result and error. Call it in `CullerService::analyses`, `photo_analysis`, `photo_issues`, `similar_groups` and
(P3) the triage build. Old rows stay in the table untouched; they're just not read. Re-running a step overwrites.

### 1.4 `CullerService::analyze` on the pipeline

Keep its signature, `AnalyzeRequest` and `AnalyzeReport`. New body:

1. `pipeline = Pipeline::from_analyzers(self.analyzers.read().clone())`. If `req.kinds` is not empty, keep only
   steps whose outputs include one of those kinds (`SubjectsStep` counts for any of its five).
2. Photos from `photos_for(...)` (unchanged); `index = store.result_index(&ids)`; per photo
   `stale_steps(...)`; photos with none → `up_to_date`.
3. For each chunk of `ANALYZE_BATCH` (16), in the existing rayon pool of `ANALYZE_THREADS` (4):
   - `self.previews.get(photo, None, MASTER_EDGE)`: **`None` for the source**, so a photo without a cached
     master fails with "no preview cached" instead of reading the original. Report it in `failed`; don't store it.
   - `AnalysisImage::from_jpeg(bytes)` once; `run_photo(...)` with the service's stop flag (P2 adds it; in P1
     pass a fresh `AtomicBool::new(false)`).
4. Save the chunk in one transaction with `save_results`; count `kind_errors` from `Err` results; emit
   `AnalysisProgress` as today.

Delete what this replaces: the `prior` loading of faces/saliency in `analyze()` (steps don't read stored inputs).
`AnalysisKind::inputs()` stays: `Analyzers::run` uses it inside `SubjectsStep`.

### 1.5 Tests (P1)

- `pipeline.rs` unit tests with fake steps: a step with no stored rows is stale; matching implementation is not;
  a changed implementation is; `force` makes everything stale; an unknown stored kind is ignored; a newly pushed
  step is stale for already-analysed photos while the others are not; `run_photo` stops before the next step
  when the flag is set.
- `SubjectsStep` with `FakeFaces` (exists in `analysis/mod.rs` tests): returns faces, eyes, faceFraming and
  subjectFocus results in one call.
- `retain_current`: a result stored with an old implementation is cleared.
- Existing tests in `crates/cull-platform-fs/tests/analysis.rs` and the `analyze` example must pass unchanged.
- New: analysing a photo whose preview was cleared, with its source still connected, reports it in `failed` and
  opens the source zero times (use the open counter in `source_safety.rs`).

**Done when:** all tests pass; `cargo run -p cull-platform-apple --example analyze -- test-photos` prints the
same results as before.

---

## P2 — Background analysis during import, with Stop

### 2.1 `worker.rs`

```rust
pub(crate) struct AnalysisWorker {
    wake: (Mutex<bool>, Condvar),   // "there may be work"
    pub stop: AtomicBool,           // set by stop_analysis; sticky until resume or restart
    pub running: AtomicBool,        // a pass is in progress (for the UI)
}
```

- `CullerService` gains `analysis: AnalysisWorker` (initialised in `with_store`; nothing starts).
- `pub fn start_analysis_worker(self: &Arc<Self>, sink: Arc<dyn EventSink>)`: spawns one `std::thread`
  (named `"analysis"`) that loops forever:
  1. wait on the condvar until woken (it starts woken, so the first pass resumes unfinished work at launch);
  2. if `stop` is set, go back to waiting;
  3. `running = true`; `self.analyze_pending(&*sink)`; `running = false`; log errors with `eprintln!`, never panic;
  4. go back to waiting.
  Call it at most once (guard with a `OnceLock`/`AtomicBool`); a second call is a no-op.
- `pub fn wake_analysis(&self)`: set the flag and `notify_one`.
- `pub fn stop_analysis(&self)`: set `stop`. `pub fn resume_analysis(&self)`: clear `stop`, then `wake_analysis()`.
- `pub fn analysis_stopped(&self) -> bool` for the UI to show the right button after a reload.

### 2.2 `analyze_pending`

`pub fn analyze_pending(&self, sink: &dyn EventSink) -> Result<AnalyzeReport>` = `analyze` with
`AnalyzeRequest::default()` (every photo of the session's sources, all steps) and the worker's `stop` flag passed
to `run_photo`. It processes each photo at most once per pass, so photos it can't load (no cached preview) are
skipped until the next wake, never retried in a loop. Results already saved stay when it stops.
Make the manual `analyze()` use the same stop flag (it then returns early while analysis is stopped; say so in its
doc comment).

### 2.3 Waking from the scan

`CullerService::scan` wraps its sink:

```rust
struct WakeOnSaved<'a> { inner: &'a dyn EventSink, svc: &'a CullerService }
impl EventSink for WakeOnSaved<'_> {
    fn emit(&self, e: CoreEvent) {
        if matches!(e, CoreEvent::ScanSaved { .. }) { self.svc.wake_analysis(); }
        self.inner.emit(e);
    }
}
```

and calls `wake_analysis()` once more when the scan ends (backfilled previews may have made photos analysable).
`scan.rs` is not touched.

No Clear-previews lock is needed: analysis never falls back to originals, so a preview deleted mid-run only means
that photo is skipped until the next scan rebuilds it.

### 2.4 Tauri + UI

- `src-tauri/src/lib.rs` `setup`: after `set_analyzers` and `set_preview_maker`, call
  `svc.start_analysis_worker(Arc::new(TauriSink(app.handle().clone())))`.
- Commands: `stop_analysis`, `resume_analysis` (sync, like `cancel_scan`), `analysis_stopped -> bool`.
- `CullerApi`: `stopAnalysis(): Promise<void>`, `resumeAnalysis(): Promise<void>`, `analysisStopped(): Promise<boolean>`.
  `mockApi`: keep a `stopped` flag; when not stopped, emit a fake `analysisProgress` run after `scanSaved`.
- `Home.svelte`: listen to `analysisProgress` (already emitted). While `done < total`, show
  "Analysing photos · 340 / 2,000" next to the scan status with a **Stop analysis** button. When stopped, show
  "Analysis stopped · Resume analysis". Read `analysisStopped()` on mount.

### 2.5 Tests (P2)

- With the worker started on an in-memory service and the test-photo fixture: after `scan`, all photos end up
  analysed (poll `analyses()` with a 30 s timeout).
- Stop before scanning → after the scan nothing is analysed; `resume_analysis` → everything is.
- Restart (new service on the same DB file) with half the photos analysed → the first pass finishes the rest,
  and photos already analysed aren't recomputed (`AnalyzeReport.up_to_date`).
- `source_safety.rs`: a full scan + background analysis opens each original exactly once (the scan read).

---

## P3 — Triage plan (computed on request)

### 3.1 Grouping rule

`GroupingOptions` gets `burst_gap_secs: Option<i64>` (serde default `None`, so `similarGroups` behaves as today).
In `linked()`: if both times exist and `|Δt| <= burst_gap_secs`, return `true` before any other check (frames
seconds apart always belong together, even if the subject moved). Triage uses
`GroupingOptions { burst_gap_secs: Some(moments::BURST_GAP_SECS), ..Default::default() }`.

### 3.2 Types (`triage/mod.rs`)

```rust
pub const PLAN_VERSION: &str = "triage.v1"; // bump whenever routing rules or thresholds change

#[serde(rename_all = "camelCase")] pub enum Question { Judge, Pick, Sweep }
#[serde(rename_all = "camelCase")] pub enum GroupKind { Burst, Similar }

#[serde(rename_all = "camelCase")]
pub struct TriageGroup {
    pub id: PhotoId,          // the group's first photo in capture order
    pub kind: GroupKind,      // Burst when every consecutive gap is <= BURST_GAP_SECS, else Similar
    pub size: u32,
    pub rank: u32,            // 0 = best quality_score in the group
    pub suggested: bool,      // ★: rank 0 AND this frame has no reject-level issue
}

#[serde(rename_all = "camelCase")]
pub struct TriageItem {
    pub photo_id: PhotoId,
    pub question: Question,
    pub group: Option<TriageGroup>,
    pub reasons: Vec<IssueKind>,  // Sweep: why, worst first. Pick: this frame's reject-level issues (red underline)
    pub hints: Vec<IssueKind>,    // badges only
    pub score: Option<f32>,       // quality_score, for the outcome log
    pub analyzed: bool,           // false = no current results yet → Judge, no badges
}
```

`Plan` (model.rs) gains `triage: Vec<TriageItem>` (same length and order as `photos`) and
`triage_version: String`. Mirror in `ui/src/lib/api/types.ts`.

### 3.3 `triage::build(photos: &[Photo], analyses: &HashMap<PhotoId, PhotoAnalysis>) -> Vec<TriageItem>`

Inputs are already current-only (`retain_current`). Steps, in this order:

1. Per photo: `analyzed` = it has at least one result; `issues(a)` (existing); `score = quality_score(a)`.
2. **Accidental** (an `Accidental` issue) → `Sweep`, `reasons = [Accidental]`. These photos are left out of
   grouping.
3. **Group** the rest (analysed ones only) with `similar_groups` and the triage options. Every member → `Pick`,
   `group` filled from the `SimilarGroup` (it's already sorted best first: index = rank). Group id = member with the
   earliest `captured_at` (ties: lowest id). `reasons` = the frame's reject-level issues.
4. Not grouped and has a reject-level issue with `severity >= SWEEP_MIN_SEVERITY` → `Sweep`, `reasons` = its
   reject-level issues, worst first.
5. Everything else → `Judge`.
6. `hints` for every item = all its issues that are not in its `reasons` (so a borderline-dark photo that stays a Judge still shows a badge).

`triage/route.rs` holds the constants:

```rust
pub const REJECT_KINDS: &[IssueKind] = &[Blurry, MotionBlur, SubjectOutOfFocus, Underexposed, Overexposed, FaceCutOff, Utility];
pub const SWEEP_MIN_SEVERITY: f32 = 0.4;
```

Eyes closed is a hint, never a Sweep reason. A group whose frames are all bad stays a Pick (no member gets ★).

### 3.4 `CullerService::plan`

After the existing moments/bursts: `load_analyses(&ids, None)` (all kinds; grouping needs the similarity
embeddings), `retain_current` on each, then `triage::build`. Set
`triage_version = PLAN_VERSION`. Keep `bursts` in `Plan` for now (the UI stops using them in Part B).

### 3.5 Tests (P3)

- `triage` unit tests with hand-built `PhotoAnalysis` values: accidental inside a burst is swept and the burst
  keeps the rest; a 3-frame burst with one blurry frame → Pick, blurry frame has `reasons=[Blurry]`, ★ on the best
  sharp frame; an all-blurry burst → Pick, no ★; a lone blurry photo → Sweep; eyes closed alone → Judge with a hint;
  two near-identical photos 5 minutes apart → Pick with `GroupKind::Similar`; unanalysed → Judge, `analyzed=false`.
- `grouping.rs`: `burst_gap_secs` links frames 2 s apart with very different hashes; `None` keeps today's behaviour
  (the existing `groups_by_hash_and_time_best_first` test must still pass).
- **Timing test**: in-memory store with 2,000 photos over 5 days, every photo with blur, exposure, hash and a
  64-float embedding stored; `plan()` over all of them must finish in under 2 s in a debug build (print the time).
  If it fails, report it; don't add a cache without asking.

---

## P4 — Outcome log

### 4.1 Table (`store.rs`, created with the others)

```sql
CREATE TABLE IF NOT EXISTS outcomes (
    id           INTEGER PRIMARY KEY,
    photo_id     INTEGER NOT NULL REFERENCES photos(id),
    plan_version TEXT NOT NULL,
    question     TEXT NOT NULL,     -- judge | pick | sweep
    place        TEXT NOT NULL,     -- timeline | openedEarly | rescue
    suggestion   TEXT NOT NULL,     -- the TriageItem shown, as JSON
    decision     TEXT NOT NULL,     -- like | dislike | favorite | hate (what the user chose)
    at           TEXT NOT NULL
);
```

### 4.2 API

```rust
#[serde(rename_all = "camelCase")] pub enum Place { Timeline, OpenedEarly, Rescue }
#[serde(rename_all = "camelCase")]
pub struct OutcomeEntry { pub item: TriageItem, pub plan_version: String, pub place: Place, pub decision: Decision }
pub fn log_outcomes(&self, entries: &[OutcomeEntry]) -> Result<()>  // one transaction
```

Tauri command `log_outcomes`; `CullerApi.logOutcomes(entries)`; mock: no-op that resolves.
Rows are never updated or deleted; an undo is not logged (the latest row for a photo is the final answer).
A Skip is not a decision and is not logged.

### 4.3 Tests

Insert three entries, read them back with a test-only query, check JSON round-trips and `place` values.

---

## Part B — UI

Build against the mock first (`npm run dev:mock`), then check in the real app. Visual reference: the prototype
at https://claude.ai/artifact/DAwoVDSgqF72HdcwxbAzbH. Use tokens from `app.css` (`--like`, `--favorite`,
`--hate`, `--dislike`, `--accent`); add `--pick` (blue) and use `--hate` for set-aside red.

### B1 — Mock + pure helpers

- `api/mockTriage.ts`: deterministic `triageFor(photos): TriageItem[]` using the same `rng` as
  `mockAnalysis.ts`: ~10 % Sweep with mixed reasons, bursts from the existing mock `bursts` become Pick groups
  (one all-bad), a few Similar groups, hints on ~15 % of Judge items. `mockApi.plan` returns it.
- `lib/triage.ts` (pure, no Svelte), the only place that knows the navigation rules:
  - `isSetAside(t)`: `t.question === 'sweep'`.
  - `groupMembers(triage): Map<groupId, number[]>`: photo indexes in capture order.
  - `stopOf(i)`: the index the cursor sits on for photo `i`: the group's first member for group members,
    `i` otherwise.
  - `isStop(i)`: not set aside, and not a non-first group member.
  - `stopDone(i, photos)`: a single is done when decided; a group when all its members are decided.
  - `nextStop(from, ...)` / `firstUndecidedFrame(groupId, ...)`.
  - `setAside(photos, triage)`: indexes of undecided set-aside photos.

### B2 — Filmstrip (`Filmstrip.svelte`)

New prop `triage: TriageItem[]`. For each index in the window:
- set aside → a **red sliver** (`width: 7px; height: ~0.8×size; background: var(--hate)`), dashed outline
  instead when decided; `data-i={i}`, title "Set aside · blurry · 10:04. Click to open".
- first member of a group → a **burst stack**: up to 5 slices (`width ≈ size/4`, overlapping by 2–3 px, last
  slice wider), blue tint background, `×N` badge, `data-i={first index}`. Current when the cursor is on it.
- other group members → not rendered (they live in the stack).
- everything else → today's thumbnail.
Remove the old `burstSizeAt` prop and `×N` badge (bursts now come from triage). Keep moment dividers and drag-select.

### B3 — `BurstStrip.svelte` (new)

Props: `photos`, `indexes` (the group's members in capture order), `frame` (selected index), `triage`, `edge`,
`onPick(i)`. A centered row of small thumbnails (≈54×40, 6 px gap, blue-tinted rounded panel) between the photo and
the filmstrip. Each frame: decision outline + `Badge`, ★ when `suggested`, 3 px red underline when `reasons` is not
empty, ring on `frame`. Below: "2 of 6 decided · click a frame to jump · ★ is a suggestion". Compact layout: same
row, smaller thumbs, horizontal scroll.

### B4 — Cull navigation (`Cull.svelte`)

State: `triage` (from `plan`), `frame: number | null` (selected member when the cursor is on a group),
`opened: number | null` (a set-aside photo opened from the filmstrip). Derived: `shown = opened ?? frame ?? idx`;
the photo, `SwipeCard`, side panel and badges all use `photos[shown]`.

- **Moving:** replace `nextUndecided` with `nextStop`. Landing on a group sets `frame = firstUndecidedFrame`.
- **Deciding** (`decide(d)`, keys, swipes, round buttons, unchanged keys):
  - `opened` set → decide that photo, log `place: openedEarly`, then `opened = null` (cursor unchanged).
  - on a group → decide `frame`; next undecided member after it, else the next stop.
  - single → as today. Log `place: timeline` for every decision on a photo that has a triage item.
- **Skip** (space): on a group → next member (after the last → next stop); single → as today.
- **`[` / `]`**: previous / next frame in the group. Clicking a BurstStrip frame sets `frame`.
- **Filmstrip click** (`jump(i)`): set aside → `opened = i`; group member → `idx = stopOf(i)`, `frame = i`;
  else as today.
- **Esc:** `opened` → close it (back to the cursor); else today's behaviour.
- **Undo:** if the restored photo is set aside → `opened` = it; group member → cursor to its group, `frame` = it.
- **Shift+arrow (whole moment) and drag-select decisions skip set-aside photos.** They stay for Rescue.
- **Banners above the photo:** opened set-aside photo → red banner "Set aside · <reasons>. Decide now or leave it
  for Rescue" + "Leave for Rescue (esc)". All-bad group → blue banner "Every frame has issues. It stays here
  because you shot a burst."
- **Hints:** small gold pills on the photo for `hints`; red pills for an opened photo's `reasons`.
- **Top bar:** "N set aside for the end" (undecided set-aside count) when N > 0.
- **Done state:** when no stop is left, the existing done card adds "N photos set aside" and a primary
  **Open Rescue** button (before "Review & export"). With N = 0 it's unchanged.
- `MomentsBar`: count set-aside photos as not part of a moment's progress (a moment is full when its non-set-aside
  photos are decided). No other change.

### B5 — Rescue screen (`Rescue.svelte`, `App.svelte` view `'rescue'`)

Props: `range`, `onBack` (to Cull), `onDone` (to Review). Loads `plan` for the range; shows the undecided set-aside
photos. Renders through `AppShell` like every screen.
- **Two rows of multi-select chips** (existing `.chips` / `.chip.on` styles): top = events/days (calendar tags
  from `api.tags()` that overlap the range, else one chip per day), bottom = reasons present (labels from an
  `ISSUE_LABELS: Record<IssueKind, string>` map added to `lib/triage.ts`: "blurry", "motion blur", "too dark"…;
  the same map feeds the banners and pills in B4). Each chip shows a count. **No chip selected in a row = no filter for that row.**
- Grid of square thumbnails (`minmax(104px, 1fr)`), neutral, reason pill bottom-left. Click cycles
  ✓ Keep → ★ Favorite → ⊘ Hate → clear (outline + badge). Space (Mac) / long-press (phone) enlarges the hovered or
  pressed photo in an overlay; Esc closes.
- Footer: "Done: 2 Keep, 1 Favorite, 5 Nope" (counts over **all** set-aside photos, not just the filtered ones).
  Done → `decide_many` per decision (cleared ones as `dislike`), `logOutcomes` with `place: rescue` for all of them,
  then `onDone`. Leaving without Done decides nothing.

### B6 — Hooking up outcomes

Every place that decides a photo with a triage item calls `api.logOutcomes` after `api.decide` resolves (inside
the `enqueue` chain so ordering stays right), with the item, `plan.triageVersion`, the place and the decision.

### B7 — Checks

Mock browser: walk the prototype's scenario (single → hint → set-aside skipped → burst frames → open a red sliver
→ all-bad burst → end → Rescue with filters → Done) at desktop and phone width, light and dark. Then the real app
on `test-photos/`. `npm run check` clean.

---

## Cut (and why)

| Idea | Why it's out |
|---|---|
| Dependency graph / per-analyzer parallelism inside one photo | Photos in parallel already use every core. Dependent analyzers are one step instead. |
| Version history (keep old results next to new ones) | Schema migration for little value now. Register a new engine under a new step id to compare. |
| Wide SQL view, one column per score | Low value. |
| Stored groups / suggestions tables, regroup triggers | Computing on request is fast enough (timing test) and can't go stale. |
| Splitting time clusters by looks | Rare problem; revisit with outcome data. |
| Selected-days-first priority, pause, lower thread priority | Every photo gets analysed anyway. |
| Clear-previews lock for analysis | Not needed once analysis can't read originals. |
| Separate set-step trait / registered grouping stages | One consumer; plain functions are easier to change. |
| Logging photos that were shown but not decided, time per photo | Low value now. |

## Order

P1 → P2 → P3 → P4 → B1 → B2–B4 → B5 → B6 → B7, one commit per phase. B1–B3 can start in parallel with P2–P4 once
P3's types (3.2) are written, since the UI only needs the mock.
