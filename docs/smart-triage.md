# Smart triage (agreed design, 2026-11-10)

How photo analysis (see [photo-analysis.md](photo-analysis.md)) plugs into culling. Not built yet.
Wireframes of the four options considered: https://claude.ai/artifact/DAwoVDSgqF72HdcwxbAzbH
(we chose **Option A + the rescue screen from Option B**; Option D "review the draft" is the long-term goal).

## One vocabulary

Every screen uses the app's four decisions with the same words, icons and keys: ✕ Nope (←), ⊘ Hate (↓),
★ Favorite (↑), ✓ Keep (→), plus Skip (space). Never invent screen-specific verbs ("keep none", "toggle",
"rescue" as an action) for the same decision.

## Concept: three questions + hints

Analysis results are evidence for one of three questions. Each is a **pre-answered set**: photos + a
default answer the user corrects. One component, three uses.

| Question | Asks | Default | Fed by |
|---|---|---|---|
| **Pick** | "Which of these do you want?" | nothing pre-selected; ★ marks the suggested best frame | bursts, near-duplicate groups |
| **Sweep** | "These look bad. Rescue any?" | none kept (shown neutral, see below) | accidental, blurry, motion blur, subject out of focus, exposure, face cut off, utility |
| **Judge** | "Keep this one?" | undecided | everything else (today's swipe) |
| *Hints* | never a question | — | tilt, colour cast, noise, flat, low aesthetic → badges only |

## Routing: each photo is asked exactly once (in order)

1. **Accidental → Sweep**, even inside a burst (removed from the burst first).
2. **In a burst / near-duplicate group → Pick, stays in the timeline.** Its issues only lower its rank and
   show as badges. **A burst where every frame is bad stays in the timeline as a Pick**; NOT auto-moved to Sweep — a burst means the owner was trying to capture something. Revisit later.
3. **Reject-level issue → Sweep**, filed under its worst issue.
4. Else **Judge**, with hint badges.

**Eyes closed is NOT a Sweep reason for now** (candids; detector untested on real faces). It is only a
ranking signal in Pick and a badge in Judge. Revisit once outcome data exists.

## Flow (Option A + B's rescue screen)

```
Pick days → Cull queue in timeline order:
   single photo  → Judge card (today's swipe + hint badges)
   burst/group   → Pick card: big photo = selected frame, swiped with the normal keys; click a frame or [ ] to jump
   sweep photos  → pulled out; filmstrip/moments bar shows a thin "3 set aside for the end" marker
→ end of session: ONE Rescue screen for the whole session
```

### Cull screen changes (the UI otherwise stays as it is)

1. **Burst stack in the filmstrip.** A burst is drawn as a tight stack of thin overlapping slices with a
   `×N` count and a blue tint, so you can see it coming.
2. **Burst sub-timeline.** When a burst is current, the stage shows the selected frame plus a small
   second timeline of the burst's frames, between the photo and the main filmstrip. **Nothing is
   pre-selected**: every frame starts undecided. The big photo is the selected frame and takes the same
   decisions as any photo (← Nope, ↓ Hate, space Skip, ↑ Favorite, → Keep); deciding moves to the next
   frame, and after the last frame the cursor leaves the burst. Click a frame (or `[` `]`) to jump.
   ★ = suggested best (a hint only), red underline = frame has an issue, each frame shows its decision badge.
   No burst-only buttons (no "keep none", "toggle" or "done").
3. **Red set-aside sliver.** A swept photo stays in its timeline position as a thin red rectangle.
   Swiping skips it automatically. Clicking it forces it open with a "Set aside · <reason>" banner:
   Nope / Hate / Favorite / Keep decides it there and removes it from Rescue; `esc` leaves it for Rescue and
   returns to the cursor. Decided slivers turn dashed (or green if kept). The outcome log records that
   it was opened early, which is a rescue signal.

### Rescue screen

- Grid of thumbnails, **not X'd out** (neutral, to avoid rubber-stamping).
- Click → ✓ Keep (green). Again → ★ Favorite (gold). Again → ⊘ Hate (red). Again → clear.
- Enlarge: long-press on phone; space (Quick Look style) on Mac.
- **Two rows of multi-select filter chips:**
  - top row: calendar events / days
  - bottom row: issue reasons (blurry, motion, dark, bright, pocket, cut off, …)
  - **all chips deselected = everything.** Selecting chips narrows (e.g. "these two days, blurry only").
- Final button: "Done: 2 Keep, 1 Favorite, 5 Nope" — only then are the cleared ones recorded as Nope
  (never deleted). Quitting before this leaves them undecided.

### Analysis timing

Start analysis as soon as days are picked, in timeline order, staying ahead of the swipe cursor
(~1 min / 1,000 photos). User can start swiping immediately.

## Data model (see [triage-build-plan.md](triage-build-plan.md))

1. **Signals** — the existing `analysis` table, one row per photo and result kind, tagged with the engine id.
   Only results whose engine id matches the registered step are read; old ones stay, ignored.
2. **Triage plan** — computed on request from signals (`triage_plan`), not stored. Groups (bursts and
   near-duplicates), rank, ★, question and reasons per photo. Fast enough to recompute every time.
3. **Outcomes** (new table) — one row per decision on a suggested photo: the suggestion snapshot, plan version,
   final decision and where it was decided (timeline / opened early / Rescue). Used to measure Sweep rescue rate
   per issue, how often the ★ frame is kept, Nopes on photos with no issues, and pace. Path to Option D:
   auto-accept suggestion types whose measured accuracy is high enough.
