//! Any source folder, any destination folder: whatever either looks like inside, the plan says exactly
//! which files are already on the drive (anywhere, under any name) and which are not, and the run copies
//! exactly the missing ones to the right place, once.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use cull_core::{CullerService, NullSink};
use cull_platform_fs::{FsLibrary, FsSource};

mod common;
use common::*;

/// Source files: (path in the source, bytes). Nothing like a camera card's layout.
fn messy_source(src: &Path) -> Vec<(&'static str, Vec<u8>)> {
    let files = vec![
        ("top-level.jpg", jpeg("FUJIFILM", "2026:10:05 10:00:00", 1)),
        ("Trip dump/day one/IMG 0001.JPG", jpeg("FUJIFILM", "2026:10:05 11:00:00", 2)),
        ("Trip dump/day one/IMG 0001.RAF", raf("2026:10:05 11:00:00", 2)),
        ("Trip dump/día dos — 京都/DSCF0002.jpeg", jpeg("FUJIFILM", "2026:10:06 09:00:00", 3)),
        ("misc/a/b/c/d/DSC_0003.NEF", nef("2026:11:03 15:00:00", 4, 300_000)),
        ("misc/a/b/c/d/DSC_0003.JPG", jpeg("NIKON CORPORATION", "2026:11:03 15:00:00", 4)),
        ("misc/clip.MOV", noise(400_000, 5)),
        ("misc/clip2.mp4", noise(350_000, 6)),
        ("same name/DSC_0001.JPG", jpeg("NIKON CORPORATION", "2026:11:04 10:00:00", 7)),
        ("same name/again/DSC_0001.JPG", jpeg("NIKON CORPORATION", "2026:11:04 10:05:00", 8)),
        ("phone/IMG_5000.HEIC", noise(120_000, 9)),
        ("notes.txt", b"not a photo".to_vec()),
    ];
    for (p, b) in &files {
        put(&src.join(p), b, Some("2026-11-05T12:00:00"));
    }
    files
}

fn bytes<'a>(files: &'a [(&'static str, Vec<u8>)], path: &str) -> &'a [u8] {
    &files.iter().find(|(p, _)| *p == path).unwrap().1
}

struct Messy {
    e: Env,
    files: Vec<(&'static str, Vec<u8>)>,
    /// Source paths that are on the drive before the first run.
    already: BTreeSet<&'static str>,
}

fn messy() -> Messy {
    let e = env();
    fs::remove_dir_all(&e.card).unwrap();
    let files = messy_source(&e.card);
    let d = &e.ssd;
    // Already on the drive, in every awkward way:
    put(&d.join("2026/whatever/renamed completely.jpg"), bytes(&files, "top-level.jpg"), None); // other name
    put(&d.join("Backups/old laptop/Pictures/x/y/z/IMG 0001.RAF"), bytes(&files, "Trip dump/day one/IMG 0001.RAF"), None); // deep
    put(&d.join("DSCF0002.JPG"), bytes(&files, "Trip dump/día dos — 京都/DSCF0002.jpeg"), None); // top, other extension case
    put(&d.join("dupes/one/clip.MOV"), bytes(&files, "misc/clip.MOV"), None); // twice on the drive
    put(&d.join("dupes/two/clip copy.mov"), bytes(&files, "misc/clip.MOV"), None);
    put(&d.join("Photos/Nikon/2026/2026-11/DSC_0001.JPG"), bytes(&files, "same name/again/DSC_0001.JPG"), None); // where we'd put it
    // Lookalikes that are NOT backups:
    let nef_bytes = bytes(&files, "misc/a/b/c/d/DSC_0003.NEF");
    put(&d.join("truncated/DSC_0003.NEF"), &nef_bytes[..nef_bytes.len() / 2], None); // a copy that was cut off
    let mut other = bytes(&files, "misc/clip2.mp4").to_vec();
    other[10] ^= 0xFF;
    put(&d.join("lookalike/clip2.mp4"), &other, None); // same name and size, other content
    put(&d.join("empty/IMG_5000.HEIC"), b"", None); // same name, empty
    put(&d.join("unrelated/holiday.jpg"), &jpeg("Canon", "2020:01:01 10:00:00", 99), None);
    put(&d.join("docs/readme.txt"), b"hello", None);
    let already = ["top-level.jpg", "Trip dump/day one/IMG 0001.RAF", "Trip dump/día dos — 京都/DSCF0002.jpeg", "misc/clip.MOV", "same name/again/DSC_0001.JPG"].into();
    Messy { e, files, already }
}

fn plan_at(svc: &Arc<CullerService>, src: &Path, dest: &Path, full: bool) -> cull_core::offload::OffloadPlan {
    svc.offload_plan(
        Arc::new(FsSource::new(src).unwrap()),
        src.to_string_lossy().into_owned(),
        Arc::new(FsSource::new(dest).unwrap()),
        Arc::new(FsLibrary::allowing_same_volume_for_tests(dest, src).unwrap()),
        full,
        &NullSink,
    )
    .unwrap()
}

/// Every photo/video of the source is on the drive with identical bytes; returns how many drive files hold each.
fn copies_on_drive(m: &Messy) -> Vec<(&'static str, usize)> {
    let drive: Vec<Vec<u8>> = snapshot(&m.e.ssd).keys().map(|k| fs::read(m.e.ssd.join(k)).unwrap()).collect();
    m.files.iter().filter(|(p, _)| *p != "notes.txt").map(|(p, b)| (*p, drive.iter().filter(|d| *d == b).count())).collect()
}

#[test]
fn tells_exactly_what_is_and_is_not_on_the_drive_then_copies_only_the_rest() {
    for full_check in [false, true] {
        let m = messy();
        let e = &m.e;
        let drive_before = snapshot(&e.ssd);
        let source_before = snapshot(&e.card);

        // What it tells you before anything is written.
        let p = plan_full(e, fs_lib(e), full_check);
        let media = m.files.len() as u64 - 1; // all but notes.txt
        assert_eq!(p.already_files, m.already.len() as u64, "already on the drive");
        assert_eq!(p.new_files, media - m.already.len() as u64, "not on the drive yet");
        assert_eq!(p.skipped.iter().map(|s| (s.extension.as_str(), s.count)).collect::<Vec<_>>(), [("TXT", 1)]);
        let found: Vec<(&str, u64)> = p.already_in.iter().map(|a| (a.folder.as_str(), a.files)).collect();
        assert_eq!(found.iter().map(|f| f.1).sum::<u64>(), p.already_files);
        for folder in ["", "2026/whatever", "Backups/old laptop/Pictures/x/y/z", "Photos/Nikon/2026/2026-11"] {
            assert!(found.iter().any(|f| f.0 == folder), "found in {folder:?}: {found:?}");
        }
        assert!(found.iter().any(|f| f.0.starts_with("dupes/")));
        assert_eq!(snapshot(&e.ssd), drive_before, "planning writes nothing to the drive");

        // The copy: exactly the missing ones.
        let r = start(e);
        assert!(r.all_safe, "{r:?}");
        assert_eq!((r.copied, r.already), (p.new_files, p.already_files));
        for (path, n) in copies_on_drive(&m) {
            let want = match path {
                "misc/clip.MOV" => 2, // was there twice; not copied a third time
                _ => 1,
            };
            assert_eq!(n, want, "{path}: copies on the drive");
        }
        // To the right places: brand / year / trip or month, by kind.
        let at = |rel: &str, src: &str| assert_eq!(fs::read(e.ssd.join(rel)).unwrap_or_default(), bytes(&m.files, src), "{rel}");
        at("Photos/Fuji/2026/Japan trip/IMG 0001.JPG", "Trip dump/day one/IMG 0001.JPG");
        at("Photos/Nikon/2026/2026-11/RAW/DSC_0003.NEF", "misc/a/b/c/d/DSC_0003.NEF");
        at("Photos/Nikon/2026/2026-11/DSC_0003.JPG", "misc/a/b/c/d/DSC_0003.JPG");
        at("Photos/Nikon/2026/2026-11/DSC_0001-1.JPG", "same name/DSC_0001.JPG"); // the name was taken by the other DSC_0001
        at("Photos/Unknown camera/2026/2026-11/Videos/clip2.mp4", "misc/clip2.mp4");
        at("Photos/Unknown camera/2026/2026-11/HIF/IMG_5000.HEIC", "phone/IMG_5000.HEIC");
        // Nothing that was on the drive changed; the source is untouched.
        let after = snapshot(&e.ssd);
        for (path, v) in &drive_before {
            assert_eq!(after.get(path), Some(v), "{path} on the drive changed");
        }
        assert_eq!(snapshot(&e.card), source_before);

        // Again: nothing left to do, whichever way it's asked.
        for full in [false, true] {
            let p = plan_full(e, fs_lib(e), full);
            assert_eq!((p.new_files, p.already_files), (0, media), "second plan (full check {full})");
        }
    }
}

#[test]
fn picking_the_drive_or_a_folder_inside_it_gives_the_same_answer() {
    let m = messy();
    let e = &m.e;
    plan(e);
    assert!(start(e).all_safe);
    let media = m.files.len() as u64 - 1;

    // Pick the drive's Photos folder instead: only what's inside it counts as there.
    let photos = e.ssd.join("Photos");
    let inside: u64 = 7; // the 6 copied in the run + the DSC_0001 that was already in Photos
    let p = plan_at(&e.svc, &e.card, &photos, false);
    assert_eq!((p.already_files, p.new_files), (inside, media - inside), "{:?}", p.already_in);
    // New folders go straight into it: no Photos/Photos.
    assert!(p.folders.iter().all(|f| !f.path.starts_with("Photos/")), "{:?}", p.folders.iter().map(|f| &f.path).collect::<Vec<_>>());
    let r = e.svc.offload_start(&[], &NullSink).unwrap();
    assert!(r.all_safe, "{r:?}");
    assert!(!photos.join("Photos").exists());

    // Back to the whole drive, then Photos again: both say everything is there (cached answers stay right).
    for _ in 0..2 {
        assert_eq!(plan(e).new_files, 0);
        assert_eq!(plan_at(&e.svc, &e.card, &photos, false).new_files, 0);
    }

    // A file swapped on the drive for a different one of the same size is noticed (cache checks size + time).
    let target = e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0003.NEF");
    let mut b = fs::read(&target).unwrap();
    b[50] ^= 0xFF;
    fs::remove_file(&target).unwrap();
    put(&target, &b, Some("2027-01-01T00:00:00"));
    let p = plan(e);
    assert_eq!(p.new_files, 1, "the damaged copy no longer counts");
    assert!(start(e).all_safe);
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0003-1.NEF").exists());
    assert_eq!(fs::read(&target).unwrap(), b, "the damaged file is left alone, not overwritten");
}

#[test]
fn a_source_that_is_a_previous_backup_layout_works_too() {
    // Source = a folder laid out like our own output; destination = empty drive.
    let m = messy();
    let e = &m.e;
    plan(e);
    assert!(start(e).all_safe);
    let second = e.ssd.parent().unwrap().join("second drive");
    fs::create_dir_all(&second).unwrap();
    let p = plan_at(&e.svc, &e.ssd.join("Photos"), &second, false);
    assert_eq!(p.already_files, 0);
    assert_eq!(p.new_files, 7);
    let r = e.svc.offload_start(&[], &NullSink).unwrap();
    assert!(r.all_safe, "{r:?}");
    assert_eq!(plan_at(&e.svc, &e.ssd.join("Photos"), &second, true).new_files, 0);
}

/// Remembered fingerprints are stored by path relative to the pick. Picking the drive and picking its
/// Photos folder share one drive id, so `Fuji/x.jpg` can mean two different files. A remembered answer
/// for one must never be used for the other, or a photo could be called backed up when it isn't.
#[test]
fn remembered_fingerprints_never_apply_to_a_different_file_at_the_same_relative_path() {
    let m = messy();
    let e = &m.e;
    plan(e);
    assert!(start(e).all_safe);
    let photos = e.ssd.join("Photos");
    // Picking Photos remembers "Fuji/2026/Japan trip/IMG 0001.JPG" → the real copy.
    assert_eq!(plan_at(&e.svc, &e.card, &photos, false).already_files, 7);
    let real = photos.join("Fuji/2026/Japan trip/IMG 0001.JPG");
    let mtime = fs::metadata(&real).unwrap().modified().unwrap();
    // At the drive's top: a different file at that same relative path, same size, same time.
    let mut other = fs::read(&real).unwrap();
    other[20] ^= 0xFF;
    let twin = e.ssd.join("Fuji/2026/Japan trip/IMG 0001.JPG");
    put(&twin, &other, None);
    fs::File::options().write(true).open(&twin).unwrap().set_modified(mtime).unwrap();
    // The real copy is gone.
    fs::remove_file(&real).unwrap();
    let p = plan(e);
    assert_eq!(p.new_files, 1, "the photo is not on the drive any more and must be copied again");
    assert_eq!(p.folders[0].path, "Photos/Fuji/2026/Japan trip", "back into the folder made for it, though it's empty now");
    assert!(start(e).all_safe);
    assert_eq!(fs::read(&real).unwrap(), bytes(&m.files, "Trip dump/day one/IMG 0001.JPG"));
}
