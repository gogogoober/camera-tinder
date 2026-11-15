# Feedback after the first long use (14 Nov 2026)

Notes only. Nothing here is built or decided. Next step is a plan with numbered questions, then build.

Overall: the owner mostly likes the app. Previews were higher quality than expected, so exporting in the current state is fine.

## 1. Navigation (the strongest complaint)

**Problem: burst mode traps you.** Space / back keep pushing you through the burst until every frame is rated.
Going back in time and then forward gets confusing: you can't easily move forward, and revisiting photos
(especially already-rated ones) is hard.

Wanted:
- **Shift + back / Shift + enter = always linear.** Shift-back goes exactly one photo back. Shift-enter leaves a
  burst even if not every frame is rated. After jumping back to a specific photo or date, Shift-enter goes to the
  *next photo in order*, not the next one still waiting for review.
- **Plain enter inside a burst** walks the whole loop (frame to frame) and never skips ahead to "the next
  unrated frame", even when some are already liked. Reason: the owner likes, unlikes and re-likes while comparing,
  so every frame must stay reachable.

## 2. Decision vocabulary

Hesitation about Nope vs Keep: some photos should stay neutral, without counting as disliked.
Idea: left = **Neutral** instead of Nope, down stays **Hate**, so the scale reads Dislike / Neutral / Like / Love.
Decided:
- Neutral is **not** skip. It is a real rating (rated neutral), recorded like any other decision.
- **Presentation only, no database change for now.** The stored values stay `like` / `dislike` / etc.; only the
  labels, language and iconography change (the left swipe stops reading as "Nope ✕"). Same words on every screen.
Open: which stored value the left swipe maps to (today `dislike`), and what the Export filters call it.

## 2b. Culling: trackpad scroll on the timeline

In Cull (picking) mode, the picture timeline (filmstrip / moments bar) should scroll with the trackpad.

## 3. Pick screen: preview panel

The right panel under the selected-photo count and metadata is empty. Fill it with the previews of the selected
days, in a **2-column scrollable grid**.
- Click a preview to expand it in a modal.
- Seeing a day with 300 photos and wondering "what day was that?": the modal (or the grid) could add that day
  to the selection.
- **V2:** with 6,000 photos selected, let a click show just that one day.

## 4. Export: calendar tags

Calendar tags (the saved tags / calendar events) should persist on export so the Export page can filter by both
decision (Favorite / Keep / ...) and calendar tag or date. Otherwise the Export list gets huge.

## 5. Performance

Processing time was long, but it was a lot of photos, so possibly fine. Worth timing on a big library later.
