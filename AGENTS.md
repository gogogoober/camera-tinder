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

Rust + Tauri 2 + Svelte 5 (TypeScript). The rule: **all logic in `cull-core`; platforms and UI are
thin, swappable layers.**

```
crates/cull-core/          Pure Rust, NO Tauri/platform deps.
  model.rs                 Photo, Decision, Filter, Stats, DaySummary, reports (serde camelCase)
  ports.rs                 Traits: MediaSource, ExportTarget, EventSink (the platform seams)
  identity.rs              Content fingerprint: xxh3-128 of size + first 64KiB + last 16KiB
  metadata.rs              EXIF (kamadak-exif): capture time, camera
  store.rs                 SQLite (rusqlite bundled): photos, file_index, decisions, decision_log
  scan.rs                  3-pass scan: cached lookups → parallel (rayon) analyze (+ sharpness) → one txn insert
  sharpness.rs             Blur score 0–100 (variance of Laplacian on a 640px grey copy; < 22 = "soft")
  moments.rs               Group by capture-time gap: moments (15 min) and bursts (3 s)
  suggest.rs               Tag suggestions without AI: runs of shooting days, calendar events
  preview.rs               Decode → EXIF-rotate → downscale → JPEG; disk cache; capped concurrency
  api.rs                   CullerService — the ONLY facade any UI uses (plan, decide_many, tags, suggest_tags…)
crates/cull-platform-fs/   Desktop impls: FsSource (walkdir), CopyToDir export
crates/cull-platform-apple/ macOS (iOS later): EventKitCalendar (CalendarSource) + CalendarAccess permission.
                           src-tauri/Info.plist holds the calendar usage strings (embedded in dev builds too)
  tests/end_to_end.rs      Full flow on real JPEGs (scan, cull, undo, export, card→laptop move)
  examples/make_samples.rs Generates synthetic JPEGs (dates via mtime)
src-tauri/                 Thin adapter: commands → CullerService; `preview://` URI scheme;
                           lib + #[mobile_entry_point] already set up for `tauri ios init`
ui/src/lib/api/            CullerApi.ts (interface) + tauriApi.ts + mockApi.ts; index.ts picks one
ui/src/lib/components/     Home (Pick: heatmap + tags + time budget), Heatmap, Cull (loupe / card stack),
                           MomentsBar, Filmstrip, SwipeCard, Review (grid + drag-select), ExportPanel
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
npm run tauri dev                   # run the desktop app (Vite on :1420)
npm run dev:mock                    # UI only, in a browser, with the mock backend (ui-mock-alt: port 1421, alongside tauri dev)
cargo test --workspace              # core + end-to-end tests
npm run check                       # svelte-check / TypeScript
npm run build                       # frontend build → dist/
cargo run -p cull-platform-fs --example make_samples -- <dir> [count]
```

Test photos: `test-photos/` (gitignored) — 117 real Lorem Picsum images, no EXIF, mtimes spread
over 5 days with bursts. Add it in the app via ⌘⇧G in the folder picker.

## Gotchas learned

- UI says **Keep ✓** and **Nope ✕**, but the stored decisions are still `like` / `dislike` (DB compatibility).
- One screen set, two layouts: `viewport.compact` (≤760px) switches Cull/Home/Review to the phone layout
  (month calendar, card stack, round buttons). All logic is shared.
- Sharpness is computed at scan time (full decode, parallel) and backfilled for older rows on rescan.
  Plain skies/fog can score low; the UI only *suggests* rejects.
- Calendar: `EventKitCalendar` implements `CalendarSource`. The app never prompts at launch; Pick shows a
  "Connect Calendar" button (`connect_calendar` command). Events ≥21 days long are ignored; recurring events are
  de-duplicated per title+day. Mock backend fakes the connect flow.
- A burst "keep one" is two backend calls (Nope the rest, Keep one), so it takes two undos.

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
      time-budget picker, moments timeline, filmstrip, soft-photo triage, burst compare, drag-select,
      Review screen. Verified in the mock browser (`npm run dev:mock`), not yet in the real Tauri app.
- [x] Calendar tag suggestions (EventKit) — built, needs a manual check in the real app.
- [ ] GPS place names, night/colour image hints as tag sources.
- [ ] Hours-of-day timeline option; learned pace; persist dismissed suggestions.
- [ ] Verify photo-alignment fix for portrait images in the real app.
- [ ] **M2:** smart session planner (bursts/sessions/trips, time-budget batching, learned pace).
- [ ] Burst stacks + near-duplicate detection (perceptual hash).
- [ ] RAW on desktop (NEF/RAF embedded previews; `rawler` or a direct parser), RAW+JPEG pairing.
- [ ] Preview cache size management.
- [ ] iOS (`tauri ios init`, SD-card MediaSource, share-sheet export). Later: cloud sync, Android, Lightroom.

## Source safety (originals are sacred)

- The card/folder is read at exactly two moments: **scan** and **export**. Nothing ever writes to it.
  - Scan reads each new file **once** into memory (`scan::ingest`, 4 at a time) and derives fingerprint, EXIF,
    sharpness and the cached previews (2048px master + 256px thumb) from those bytes. Rescans of unchanged
    files read nothing (path+size+mtime lookup). Older rows without previews are backfilled with one read.
  - Culling never opens the source: every preview size is derived from the cached master
    (`PreviewCache::get`), so the card can be unplugged while you cull.
  - Export reads each keeper once.
- Nothing deletes, moves, renames or writes source files. Hate/Nope are rows in SQLite.
- `CopyToDir` refuses destinations inside any source ever added (`known_source_ids`, symlinks resolved), never
  overwrites (exclusive temp file + `hard_link`), and verifies each copy: full-content xxh3 of the bytes read vs
  the file on disk, plus fingerprint vs the culled photo.
- Scan reports RAW (`skippedUnsupported`) and everything else ignored (`otherFiles`: videos…) so the UI can warn
  that those are not exported.
- `crates/cull-platform-fs/tests/source_safety.rs` enforces all of this, including counting every source open
  and a static scan that fails on any new filesystem-writing call outside the audited lines.
- Preview cache costs ~0.5 MB per photo. **Clear previews** (Pick sidebar → Storage):
  - Locked while any known source is on a plugged-in **external** volume (`is_external_and_connected`: the folder
    exists and its filesystem device differs from `/`, the Data volume and $HOME). Internal folders never block.
    Core enforces this too, not only the UI.
  - Deletes only regular files directly in the cache folder named exactly like cache output (`is_cache_file`);
    never recursive, never follows symlinks, refuses if the cache folder itself is a symlink.
  - Real-volume check: `cargo test -p cull-platform-fs --test source_safety -- --ignored` (mounts a disk image).

## Conventions

- Keep `cull-core` free of Tauri/UI/platform code; add new platform behaviour behind a trait in `ports.rs`.
- Add backend features as `CullerService` methods, then a thin Tauri command, then `CullerApi`
  (+ both `tauriApi` and `mockApi`).
- Rust types crossing to the UI use `#[serde(rename_all = "camelCase")]`; mirror them in `ui/src/lib/api/types.ts`.
- Run `cargo test --workspace` and `npm run check` before calling work done.
