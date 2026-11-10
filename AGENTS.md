# Camera Tinder — agent guide

Tinder-style photo culling app. Point it at folders / SD cards, it scans and
groups photos, you swipe to decide, then export the keepers.

## Product requirements (from the owner)

- **Swipes:** ← Dislike ("nope"), → Like, ↑ Favorite, ↓ Hate. Plus undo (⌫ / ⌘Z / u) and skip (space).
- **Never delete or move originals.** Dislike/Hate are still recorded (with history) because a
  "clean up rejects" feature may come later.
- **v1 export:** copy Likes + Favorites to a chosen folder. **v2:** other actions (send to Lightroom, etc.).
- **Platforms:** macOS first. iOS later (iPhone + USB-C SD card reader; JPEG only; export via the
  share sheet; ideally no manual folder picking). Android is a nice-to-have.
- **Formats:** owner shoots mostly JPEG on **Nikon (NEF)** and **Fuji (RAF)**. Desktop should support
  RAW eventually (via embedded JPEG previews); iOS stays JPEG-only.
- **Cloud sync** (cull on phone, finish on Mac): not in v1, wanted later.
- **Smart session planner (milestone 2):** detect bursts / sessions / trips; remember what's already
  culled; ask "how much time do you have?" and size the batch (configurable target, default
  ~200–500/session; seed pace ≈ 15 min → 200 photos, 5 min → 100; learn real pace). Mid-trip
  (newest photos recent) → suggest 1–3 days at a time; post-trip → a week at a time.
- **Dedup:** exact duplicates already collapse (same content fingerprint). Near-duplicates
  (perceptual hash, e.g. `image_hasher`) and burst stacks are planned.

## Architecture

> **Photo analysis (blur, exposure, closed eyes, faces, near-duplicates, aesthetics…):** read
> [`docs/photo-analysis.md`](docs/photo-analysis.md) before touching it or building UI on it. It lists
> every feature, its trait/result type/engines, how to call it (Rust, Tauri, TS), where the files are,
> how to swap an engine, measured timings and known limits.

Rust + Tauri 2 + Svelte 5 (TypeScript). The rule: **all logic in `cull-core`; platforms and UI are
thin, swappable layers.**

```
crates/cull-core/          Pure Rust, NO Tauri/platform deps.
  model.rs                 Photo, Decision, Filter, Stats, DaySummary, reports (serde camelCase)
  ports.rs                 Traits: MediaSource, ExportTarget, EventSink, PreviewMaker (the platform seams)
  identity.rs              Content fingerprint: xxh3-128 of size + first 64KiB + last 16KiB
  metadata.rs              EXIF (kamadak-exif): capture time, camera
  store.rs                 SQLite (rusqlite bundled): photos, file_index, decisions, decision_log
  scan.rs                  List the folder → skip indexed files → read each new file once, 4 at a time,
                           saving every 32 (ScanSaved event) → cancellable (cancel_scan)
  moments.rs               Group by capture-time gap: moments (15 min) and bursts (3 s)
  suggest.rs               Day-group suggestions without AI: runs of shooting days, calendar events (one level nested)
  preview.rs               Preview cache (2048 master + 256 thumb at scan; other sizes from the master);
                           RustPreviewMaker = portable decode → rotate → downscale → JPEG
  analysis/                Static photo analysis: one module per feature (trait + result + engines), the
                           Analyzers registry, issues, near-duplicate grouping, ranking. See docs/photo-analysis.md
  api.rs                   CullerService — the ONLY facade any UI uses (plan, decide_many, suggest_tags, analyze…)
crates/cull-platform-fs/   Desktop impls: FsSource (walkdir), CopyToDir export
crates/cull-platform-apple/ macOS (iOS later): EventKitCalendar (CalendarSource) + CalendarAccess permission;
                           vision.rs = Apple Vision analysis engines (faces, eyes input, aesthetics, feature print…);
                           imageio.rs = ImageIoPreviewMaker (the macOS PreviewMaker; see Gotchas);
                           examples/analyze.rs = run/time/calibrate all analyzers on a folder;
                           examples/scan_timing.rs, preview_bench.rs = time a fresh scan / one preview
                           src-tauri/Info.plist holds the calendar usage strings (embedded in dev builds too)
  tests/end_to_end.rs      Full flow on real JPEGs (scan, cull, undo, export, card→laptop move)
  examples/make_samples.rs Generates synthetic JPEGs (dates via mtime)
src-tauri/                 Thin adapter: commands → CullerService; `preview://` URI scheme;
                           lib + #[mobile_entry_point] already set up for `tauri ios init`
ui/src/lib/api/            CullerApi.ts (interface) + tauriApi.ts + mockApi.ts; index.ts picks one
ui/src/lib/components/     Home (Pick: month calendar + suggestions + time budget), Heatmap (month calendar), Cull (loupe / card stack),
                           MomentsBar, Filmstrip, SwipeCard, Review (grid + drag-select), ExportPanel
ui/src/lib/components/layout/AppShell.svelte
                           Page frame for every screen: flat canvas + top/left/right/bottom regions,
                           main content on one raised surface; phone layout collapses to one column
ui/src/lib/select.ts       dragSelect action: drag across [data-i] items to multi-select
ui/src/lib/days.ts         day math, heat levels, time-budget → day range
```

Key design decisions:
- **Photo identity is content-based, not path-based**, so decisions survive copying a card to disk.
  Same fingerprint seen elsewhere → same photo id, location updated (`upsert_photo`).
- **Images never go through JSON commands.** The UI loads `preview://localhost/<id>?edge=N`
  (built via `convertFileSrc(id, 'preview')`), served from the Rust preview cache.
- **UI never imports Tauri directly** — only `api` from `ui/src/lib/api`. Folder picking is part of
  `CullerApi` because it's platform-specific (iOS will differ).
- Decisions/undo from the UI are serialized through a promise chain (`enqueue` in Cull.svelte) so
  the backend undo stack order stays correct.
- Escape hatch if Tauri iOS disappoints: expose `cull-core` to SwiftUI via UniFFI.
- iOS port plan: new `MediaSource` (security-scoped URL or ImageCaptureCore for the SD reader),
  share-sheet `ExportTarget`, bounded memory (embedded previews, capped decode concurrency).

Data locations (macOS): DB at app data dir `camera-tinder.db`; previews at app cache dir `previews/`
(identifier `app.cameratinder.desktop`). Preview cache has no size limit yet.

## Commands

```sh
source ~/.cargo/env                 # Rust may not be on PATH in fresh shells
npm start                           # run the desktop app (loads ~/.cargo/env, then tauri dev; Vite on :1420)
npm run dev:mock                    # UI only, in a browser, with the mock backend (ui-mock-alt: port 1421, alongside tauri dev)
cargo test --workspace              # core + end-to-end tests
npm run check                       # svelte-check / TypeScript
npm run build                       # frontend build → dist/
cargo run -p cull-platform-fs --example make_samples -- <dir> [count]
cargo run -p cull-platform-apple --example analyze -- test-photos [--rust-only|--calibrate|--pairs|--json]
cargo run --release -p cull-platform-apple --example scan_timing -- <dir> [--rust]   # ms/photo for a fresh scan
```

Test photos: `test-photos/` (gitignored) — 117 real Lorem Picsum images, no EXIF, mtimes spread
over 5 days with bursts. Add it in the app via ⌘⇧G in the folder picker.

## Gotchas learned

- UI says **Keep ✓** and **Nope ✕**, but the stored decisions are still `like` / `dislike` (DB compatibility).
- One screen set, two layouts: `viewport.compact` (≤760px) switches Cull/Home/Review to the phone layout
  (month calendar, card stack, round buttons). All logic is shared.
- The scan computes no quality scores. The old scan-time sharpness score, soft-photo triage and burst compare
  were removed (blur etc. live in photo analysis, to be rebuilt as triage). `photos.sharpness` stays in the
  schema, unused, so old databases need no migration.
- Rescans list every folder and look each file up by path + size + mtime; indexed files are never read again.
  There is deliberately **no folder-level "unchanged, skip it" cache**: it was tried (Nov 2026), saved ~25 µs per
  photo (75 ms per 3,000) and caused two bugs, so it was removed. Old databases may still have unused
  `sources.scan_*` columns. Adding a folder scans only that folder (`ScanRequest.source_ids`).
- Previews on macOS go through ImageIO (`set_preview_maker` in src-tauri), via the typed `objc2-image-io` /
  `objc2-core-graphics` bindings (no hand-written FFI; the photo's bytes are copied into ImageIO, not lent; the copy
  costs ~2 ms of ~40). ImageIO only decodes at reduced size
  when the target is ≤ 1/8 of the original; the 2048 master of a 24 MP photo still costs a near-full decode
  (~100 ms CPU), the 256 thumb is cheap. Measured on 24 MP JPEGs, 4 threads: dev build 207 → 40 ms/photo,
  release 65 → 42 ms/photo. ImageIO quality 0.7 ≈ the portable encoder's 85 in size.
- `[profile.dev.package."*"]` does not cover workspace crates: generic `image` code instantiated in `cull-core`
  (the portable preview path, analysis) runs unoptimized under `npm start` (~3× slower than release).
- Pick screen: the selected-days set in Home is the one source of truth for what gets culled; suggestion
  "Select" buttons only add/remove days in it and read their state back from it. There are no saved tags:
  they were removed (Nov 2026) as unneeded; older databases keep an unused `tags` table. Cull and Review receive
  exactly those days (`Filter.days`: any set of days, `"unknown"` = undated, absent = all). Suggestions are grouped in core
  (`suggest::from_calendar`); dismissing passes row ids back to `suggest_tags` so the rest regroups.
- Calendar: `EventKitCalendar` implements `CalendarSource`. The app never prompts at launch; Pick shows a
  "Connect Calendar" button (`connect_calendar` command). Events ≥21 days long are ignored; recurring events are
  de-duplicated per title+day. Mock backend fakes the connect flow.

- `use cull_core::*` brings in `cull_core::Result<T>`; use `std::result::Result<T, E>` explicitly
  where two type params are needed (see `CmdResult` in src-tauri).
- `[profile.dev.package."*"] opt-level = 3` is deliberate: image decoding is 10–20× slower unoptimized.
- WebKit (macOS/iOS webviews) doesn't reliably resolve percentage `max-height` inside grid/flex
  items. Photos use `.photo-fill` (absolute, 100%×100%, `object-fit: contain`) instead.
- The fingerprint does not hash the middle of the file (by design, for SD-card speed).
- Tauri app lives at repo-root `src-tauri/` (CLI convention), not `app/src-tauri`.
- RAW files are recognized (`MediaKind::Raw`) but excluded from scans via `scan::CULLABLE_KINDS`
  until embedded-preview extraction exists; scan reports them as `skippedUnsupported`.
- CSP is `null` in `tauri.conf.json` for v1; tighten later (needs `preview:` in img-src).

## Status / roadmap

- [x] **M1:** add folders → scan → swipe JPEGs (4 decisions, undo, skip, day + status filters)
      → export keepers to a folder. Decisions persist across restarts.
- [x] **UI second pass (from the design studies):** GitHub-style heatmap with tag lanes + suggestions,
      time-budget picker, moments timeline, filmstrip, drag-select, Review screen (soft-photo triage and burst
      compare were removed later; triage will be rebuilt on photo analysis). Verified in the mock browser (`npm run dev:mock`), not yet in the real Tauri app.
- [x] Calendar tag suggestions (EventKit) — built, needs a manual check in the real app.
- [ ] GPS place names, night/colour image hints as tag sources.
- [ ] Hours-of-day timeline option; learned pace; persist dismissed suggestions.
- [ ] Verify photo-alignment fix for portrait images in the real app.
- [ ] **M2:** smart session planner (bursts/sessions/trips, time-budget batching, learned pace).
- [x] Photo analysis back end (docs/photo-analysis.md): 16 analyzers (Rust + Apple Vision), stored per photo,
      issues ("eyes closed", "blurry"…), near-duplicate groups best-first. Commands + TS API + mock wired.
- [x] Faster scan: ImageIO previews, no scan-time scoring, no re-reads of indexed files, batched saves with a live
      Pick screen, Stop button. Verified with tests + mock browser, not yet in the real Tauri app.
- [ ] Photo analysis UX: when to run `analyzePhotos`, problem tabs, duplicate rows. Tune eyes/noise on real photos.
- [ ] RAW on desktop (NEF/RAF embedded previews; `rawler` or a direct parser), RAW+JPEG pairing.
- [ ] Preview cache size management.
- [ ] iOS (`tauri ios init`, SD-card MediaSource, share-sheet export). Later: cloud sync, Android, Lightroom.

## Source safety (originals are sacred)

- The card/folder is read at exactly two moments: **scan** and **export**. Nothing ever writes to it.
  - Scan reads each new file **once** into memory (`scan::ingest`, 4 at a time) and derives fingerprint, EXIF
    and the cached previews (2048px master + 256px thumb, one decode) from those bytes. Rescans list the folders;
    unchanged files read nothing (path+size+mtime lookup). Older rows without previews
    are backfilled with one read.
  - Culling never opens the source: every preview size is derived from the cached master
    (`PreviewCache::get`), so the card can be unplugged while you cull. The one fallback read (no master yet)
    checks size + fingerprint first: after a card swap another photo can sit at the same path.
  - Cached previews must start FFD8 and end FFD9; a damaged one (power loss) counts as missing and is rebuilt.
  - A crash (panic) while reading one file fails only that file; it never stops or blocks later scans.
  - Files over `scan::MAX_PHOTO_BYTES` (512 MB) are reported as failed without being read; reads never go past
    the listed size.
  - `FsSource::excluding(svc.app_dirs())` (src-tauri `add_folder`): a picked folder that contains the app's own
    database or preview cache (home folder, whole disk) never lists them.
  - Export reads each keeper once, into a destination given as a full path only.
- Nothing deletes, moves, renames or writes source files. Hate/Nope are rows in SQLite.
- `CopyToDir` refuses destinations inside any source ever added (`known_source_ids`, symlinks resolved) and on
  the same card/external drive as a connected source (so not `SD/Keepers` next to `SD/DCIM`), never overwrites
  (exclusive temp file + `hard_link`; exFAT has neither hard links nor a no-overwrite rename, so there it claims the
  name with an exclusive empty file and renames the verified copy onto it), and verifies each copy: full-content xxh3 of the bytes read vs
  the file on disk, plus fingerprint vs the culled photo.
- Scan reports RAW (`skippedUnsupported`) and everything else ignored (`otherFiles`: videos…) so the UI can warn
  that those are not exported.
- `crates/cull-platform-fs/tests/source_safety.rs` enforces all of this, including counting every source open
  and a static scan that fails on any new filesystem-writing call (incl. `fs::copy`, libc, Apple URL writers)
  outside the audited lines. `tests/failure_cases.rs` covers card swaps, clearing mid-scan, photos moved to another
  folder, crashing files, damaged previews, a card pulled out mid-read, symlinks, unusual file names, restarts
  (and `store.rs` tests: databases from earlier builds); `cull-platform-apple/tests/source_safety.rs` runs the flow with ImageIO.
- Preview cache costs ~0.5 MB per photo. **Clear previews** (Pick sidebar → Storage), refused while a scan runs:
  - Locked while any known source is on a plugged-in **external** volume (`is_external_and_connected`: the folder
    exists and its filesystem device differs from `/`, the Data volume and $HOME). Internal folders never block.
    Core enforces this too, not only the UI.
  - Deletes only regular files directly in the cache folder named exactly like cache output (`is_cache_file`);
    never recursive, never follows symlinks, refuses if the cache folder itself is a symlink.
  - Real-volume checks: `cargo test -p cull-platform-fs --test source_safety -- --ignored` (mounts exFAT disk
    images: external detection, never exporting onto the card, no overwrites on exFAT, and every raw byte of an
    exFAT and a FAT32 card unchanged after a whole session, export to a drive that fills up). macOS writes `._name`
    metadata files next to anything written to exFAT/FAT; they are its own, not the app's.

## Conventions

- Keep `cull-core` free of Tauri/UI/platform code; add new platform behaviour behind a trait in `ports.rs`.
- Add backend features as `CullerService` methods, then a thin Tauri command, then `CullerApi`
  (+ both `tauriApi` and `mockApi`).
- Rust types crossing to the UI use `#[serde(rename_all = "camelCase")]`; mirror them in `ui/src/lib/api/types.ts`.
- Every screen renders through `AppShell` (pass `top`/`left`/`right`/`bottom` snippets; the main
  content is its children, with one root element per region). Screens style their content only:
  no page backgrounds, column dividers, gutters or column grids. Layout values live in the
  "Layout" block of `ui/src/app.css` (`--canvas`, `--surface*`, `--gutter`, `--rail-*`).
  Phone-only tweaks to a screen's own content use `:global(.shell.compact) .thing`.
- Run `cargo test --workspace` and `npm run check` before calling work done.
