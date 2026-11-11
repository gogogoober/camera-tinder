# Offload: back up a card to the SSD

Status: **built (Nov 2026)**. Tested on temp folders and on real exFAT/APFS disk images; verified in the browser mock;
not yet run in the real Tauri app on a real card.

Code: `crates/cull-core/src/offload/` (kind, meta, layout, plan, run, journal, service), `crates/cull-platform-fs/src/offload.rs`
(`FsLibrary`, the drive side), Tauri commands `offload_*` in `src-tauri/src/lib.rs`, `ui/src/lib/components/Offload.svelte`
(entry: Pick screen → "Back up a card…"), browser mock `ui/src/lib/api/mockOffload.ts`.
Tests: `crates/cull-platform-fs/tests/offload.rs` (the real-volume one: `-- --ignored`).

Pick a **card** (source) and a **library** (the backup SSD). Darkroom works out which photos and videos on the card
are not on the SSD yet, wherever they sit there, shows a plan, then copies them into an organized folder tree and
proves every copy is on the SSD bit for bit. The card is only ever read.

## Rules

- **Never writes, moves, renames or deletes anything on the card.** No format button. (Same rule as culling; enforced by
  `source_safety.rs`.)
- **Never overwrites, moves or renames anything on the SSD.** Old folders, including ones made by hand, are only read.
  New files are added; a folder Darkroom made is never renamed later, even if a tag is renamed.
- **"Already backed up" is decided by content, never by name or path.** `DSC_0001.NEF` repeats every 10,000 shots.
- **A file counts as backed up only after it was read back from the SSD and matched.**
- Running it again is always safe: the plan is rebuilt and finished files are skipped.

## What gets copied

| Kind | Extensions | Goes to |
|---|---|---|
| JPEG | jpg, jpeg | the folder itself |
| RAW | nef, nrw, raf, plus dng, cr2, cr3, arw, orf, rw2 | `RAW/` |
| HEIF | hif, heic, heif | `HIF/` |
| Video | mov, mp4, m4v, avi, mts, m2ts, nev (Nikon RAW video) | `Videos/` |

Everything else is **not copied** and is listed in the report by extension: camera proxies and thumbnails (`.LRF`,
`.THM`), `.XML`, `.XMP`, card system files, hidden files. The report says "all photos and videos are backed up"
separately from "N other files were skipped".

Subfolders are created only when they get a file.

## Folder layout

```
<SSD>/Photos/
  <Brand>/
    <Year>/
      <Trip name>/          a tag or calendar event covers the photo's day
        *.JPG
        RAW/  HIF/  Videos/
      <YYYY-MM>/            no tag: one folder per month
        *.JPG
        RAW/  HIF/  Videos/
```

Example: `Photos/Fuji/2026/Japan trip/DSCF1234.JPG`, `Photos/Fuji/2026/Japan trip/RAW/DSCF1234.RAF`,
`Photos/Nikon/2026/2026-11/Videos/DSC_0500.MOV`.

- **Library root:** `<SSD>/Photos/`, picked once and remembered.
- **Brand:** from EXIF Make, normalized: `NIKON CORPORATION` → `Nikon`, `FUJIFILM` → `Fuji`, also Canon, Sony, Apple,
  Panasonic, OM System, Ricoh, Leica, DJI, GoPro. Unknown Make: the Make cleaned up. No Make at all: the brand of the
  other files in the same card folder (`100_FUJI`, `100NZ6_2`) or with the same filename prefix (`DSCF`, `DSC_`);
  otherwise `Unknown camera`.
- **Year:** the year of the photo's day; a trip goes under the year its first day is in.
- **Trip name:** among the saved tags and calendar events that cover the photo's day, the **broadest** wins (most days;
  tie: saved tag over calendar event, then earliest start, then name). Calendar events count when they are all-day or
  span several days; a timed event inside one day ("Dinner 7–9 pm") never names a folder. The plan screen offers the
  other options for each group (narrower tags, the month folder, or typed text).
- **No tag:** the month, `YYYY-MM`. Months never grow wrong, so the next offload lands in the same folder.
- **Day:** the capture time with the **6 a.m. rule**: before 06:00 counts as the previous day (also on the Pick calendar,
  see below).
- **Capture time:** EXIF `DateTimeOriginal`, the camera clock as is (the same rule as culling, so a photo's folder day
  and its Pick-calendar day agree). RAF: the EXIF of its embedded JPEG. Videos and files without EXIF: the file's
  modified time on the card (cameras write it from their own clock).
- **Folder names** are cleaned for every filesystem: `/` `:` and control characters become `-`, no leading dot, no
  trailing space or dot, at most 100 characters.
- **Folders are remembered:** the database records which folder it made for each (drive, brand, year, trip or month).
  If you rename it in Finder, the next offload finds it again by the name and size of a file it copied there; if it's
  gone, a new one is made. The drive is recognised by `Photos/_darkroom/library-id`, written on the first run.
  A same-named folder that Darkroom didn't make is used as is (files are added, nothing overwritten).
- **Mid-trip offload:** files already copied stay where they went. If a trip tag is created later, only later copies go
  into the trip folder.
- **Name clashes:** a different file with the same name gets a suffix: `DSC_0001-2.NEF`. A RAW+JPEG pair keeps the same
  suffix so the pair still matches.

## Matching: what's already on the SSD

1. List the whole picked folder with names, sizes and modified times only (no reads).
2. Fingerprint (the existing `identity.rs` fingerprint: size + first 64 KiB + last 16 KiB) **only** the SSD files whose
   size equals a card file's size. Usually a handful per card file.
3. Cache each SSD fingerprint by path + size + mtime, so the next offload reads nothing it already knows.
4. A card file with a matching SSD fingerprint is "already there" and is not copied. The first 64 KiB holds the EXIF
   timestamps to the sub-second, so two different camera files practically never share it.
5. Optional **"Check files already on the drive byte for byte"**: reads both files end to end (xxh3-128) for every
   match. The quick match can't see damage in the middle of the drive's copy (tested); this can.

The drive is remembered by its `library-id` file, not its mount path. The card needs no identity: matching is by content.

## Copy pipeline

Per file:

1. **Read the card once**, sequentially, in 4 MiB chunks, with `F_NOCACHE` (60 GB of photos must not evict
   everything else from memory). Hash while reading: xxh3-128 (the industry default: OffShoot, ShotPut Pro,
   Silverstack). The size must equal the listed size, and the fingerprint must match the plan, or the file fails.
2. **Write** to a unique `.<name>.<pid>.partial` in the target folder, created exclusively, also with `F_NOCACHE`
   so the written pages aren't left in memory.
3. **`fsync`** the file (plain, cheap).

Per batch (every ~2 GB or 200 files, and before Pause/Stop):

4. **One `F_FULLFSYNC`** on the SSD: it makes the drive write its own cache to the flash. A plain `fsync` on macOS doesn't
   (Rust's `File::sync_all` uses `F_FULLFSYNC` on Apple; that's per file and slow, so the cheap per-file sync goes
   through `libc` and only the batch boundary pays for the full flush).
5. **Verify each `.partial`**: open fresh with `F_NOCACHE`, read it end to end, compare xxh3-128 with the hash from step 1.
   Measured: an `F_NOCACHE` read is still answered from memory when the file's pages are resident (32 MiB at memory
   speed after a normal write). Writing with `F_NOCACHE` leaves no page resident, so the read-back must come from the
   drive. `verification_reads_from_the_drive_not_from_memory` checks this with `mincore` (`ru_inblock` doesn't count
   these reads).
6. **Link into place without overwriting** (existing `CopyToDir` logic: `hard_link`; on exFAT an exclusive empty claim +
   rename), then `fsync` the folder.
7. **Journal** each file `verified`.

The two halves overlap: a reader thread reads the card into a bounded queue (~64 MB in flight) while a writer thread
writes, syncs and verifies. The card never waits for the SSD. One card file is read at a time: camera files are large,
so the card's sequential speed is the limit, and a single SD card gains nothing from parallel reads. (A hidden setting
lets us measure 2 readers on a real card.) *Not built: cut until a real card says it's worth it.*

After the last batch, **re-list the card**: every photo and video on it must be either verified this run or matched
already. Anything else is listed by name in the report.

### Journal (SQLite)

- `libraries(id, volume_uuid, root_rel, name)`
- `library_files(library_id, rel_path, size, mtime, fingerprint)` the SSD fingerprint cache
- `offload_folders(library_id, brand, year, key, rel_path)` key = `trip:<name>` or `month:YYYY-MM`
- `offload_runs(id, library_id, card_volume_uuid, card_root, started_at, finished_at, status)`
- `offload_items(run_id, src_rel, size, fingerprint, hash, dest_rel, state, error)` state: `planned → written →
  verified`, or `failed`, `skipped`

Stop, a pulled card or cable, sleep or a crash leave nothing half-done under a final name. There is no separate
resume: running again plans again, finished files are found on the drive by content and skipped, and temp files an
earlier run recorded are removed first (only those exact paths, only names like `.<name>.darkroom-<tag>.partial`).
Three read or write failures in a row (card or drive unplugged) end the run; a full drive ends it at once.

### Guards

- Refuse a library inside the card, on the card's volume, or inside any culling source (existing `CopyToDir` checks).
- **Free space** checked before starting (plan size + 1 GB) and before every batch.
- **Keep the Mac awake** while a run is going (`caffeinate -i` started by src-tauri, ended when the run returns).
- **exFAT/FAT SSD:** warning that it has no journal and a power cut can damage its folders; APFS is safer.
- Card read error: the run stops, the file's partial is removed, the journal keeps everything done so far.

### Hash list on the SSD

At the end of every run (and appended per batch) Darkroom writes `Photos/_darkroom/offload-YYYY-MM-DD-HHMM.txt`:
one line per copied file, `xxh3-128  size  relative path`. It lets anyone re-check the SSD later without the app.

## Screens

- **Entry:** a "Back up a card" button on the Pick screen.
- **Choose:** the card (removable volumes, those with `DCIM` first; any folder works) and the library (remembered).
  Shows "Last backup to this SSD: Fuji, Nov 2 · Nikon, Oct 28".
- **Plan:** "1,240 new files · 38 GB · 2 cameras · 412 already on the SSD · 23 other files skipped", and the folder tree
  (brand → year → folder) with counts and sizes. Each folder row has a menu of name options. Start.
- **Copying:** progress, MB/s, time left, current file, Stop (no separate Pause: Stop, then run again).
- **Report:** "Every photo and video on this card is on the SSD, read back and matched ✓", or the list of files that
  aren't and why. Skipped files listed by type. The hash list's path.

## The 6 a.m. day, everywhere

Photos before 06:00 belong to the previous day in culling too (Pick calendar, day filters, Review, Cull, Reset days).
Built as `cull_core::days` (`photo_day`, and `SQL_PHOTO_DAY` = `date(p.captured_at, '-6 hours')` used by the store's day
filter and day list) and `photoDay()` in `ui/src/lib/days.ts`. No stored column, no migration. Decisions stay with the
photo; a 1 a.m. photo just shows under the day before.

## Code layout

- `cull-core/src/days.rs`: the 6 a.m. day.
- `cull-core/src/offload/`: `kind.rs` (file kinds and subfolders), `meta.rs` (brand, RAF EXIF), `layout.rs` (broadest
  tag, folder keys, safe names), `plan.rs` (`gather` reads card and drive; `layout` is pure), `run.rs` (the three-thread
  copy), `journal.rs` (tables), `service.rs` (`CullerService::offload_plan / offload_layout / offload_start / offload_stop`).
- `ports.rs`: `Library` + `PartialWrite` (the drive), `MediaSource::open_streaming`, `CoreEvent::OffloadProgress`.
- `cull-platform-fs/src/offload.rs`: `FsLibrary` (`F_NOCACHE`, `fsync`, `F_FULLFSYNC`, verify, no-overwrite link, statfs).

## Tests

- Plan (pure): broadest-tag choice, month fallback, 6 a.m. rule, brand fallback, clashes with pair suffixes, already-there
  by content in a differently named folder, files renamed on the SSD, two cameras with the same filenames.
- Copy: interrupted at every step then resumed; card pulled mid-file; source changed since plan; SSD full; existing
  folder not made by us; `.partial` leftovers; never overwrites.
- `source_safety.rs`: the card is opened read-only and its bytes are unchanged after a whole run (exFAT and FAT32 disk
  images), the static scan updated for the new audited write calls.
- Library on an exFAT image: no hard links path, verify still catches a planted corruption.

## Not in v1

Formatting or deleting from the card, a second backup drive in the same run, adding the SSD as a cull source
automatically, copying only keepers, renaming existing SSD folders.

## Still to do

- A real-card run in the Tauri app, timed (MB/s on the owner's reader), and a look at the screens there.
- Phone layout: the Pick screen's phone header has no "Back up a card" entry (iOS will need its own card source anyway).
