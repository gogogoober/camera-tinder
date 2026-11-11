//! Offload edge cases that could lose or corrupt data: the card changing or failing between plan and
//! copy, the drive filling up or disappearing, names taken at the last moment, crashes at every step,
//! hostile folder names, unreadable folders, odd files. Each test says what must hold.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use cull_core::offload::FolderChoice;
use cull_core::{Library, LibrarySpace, MediaEntry, MediaSource, NullSink, PartialWrite, ReadSeek, Result};
use cull_platform_fs::{FsLibrary, FsSource};

mod common;
use common::*;

// ---- a drive that misbehaves on cue -----------------------------------------

#[derive(Default)]
struct Faults {
    /// `begin` number n (1-based) and every later one fail, as if the drive was unplugged.
    begin_fails_from: Option<usize>,
    /// Writes fail with "no space left" once this many bytes were written in total.
    enospc_after: Option<u64>,
    /// `begin` panics (a bug in the app).
    panic_in_begin: bool,
    /// The flush after files got their final names panics: named on the drive, never recorded.
    panic_after_naming: bool,
    /// Pretend this much is free.
    free_bytes: Option<u64>,
    /// Another file appears at the final name just before the copy is named.
    squat_before_commit: bool,
    /// The first `begin` waits here (to start a second run meanwhile).
    hold_first_begin: Option<(SyncSender<()>, Mutex<Receiver<()>>)>,
}

struct FaultyDrive {
    inner: FsLibrary,
    root: PathBuf,
    f: Faults,
    begins: AtomicUsize,
    written: Arc<AtomicU64>,
}

fn drive(e: &Env, f: Faults) -> Arc<FaultyDrive> {
    Arc::new(FaultyDrive {
        inner: FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap(),
        root: fs::canonicalize(&e.ssd).unwrap(),
        f,
        begins: AtomicUsize::new(0),
        written: Arc::default(),
    })
}

struct FaultyPartial {
    inner: Box<dyn PartialWrite>,
    written: Arc<AtomicU64>,
    limit: Option<u64>,
}
impl PartialWrite for FaultyPartial {
    fn rel_path(&self) -> &str {
        self.inner.rel_path()
    }
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        if let Some(limit) = self.limit {
            let before = self.written.fetch_add(buf.len() as u64, Ordering::SeqCst);
            if before + buf.len() as u64 > limit {
                // Write what fits, then fail like a full disk.
                let fits = limit.saturating_sub(before) as usize;
                self.inner.write(&buf[..fits.min(buf.len())])?;
                return Err(io::Error::from_raw_os_error(28).into());
            }
        }
        self.inner.write(buf)
    }
    fn finish(self: Box<Self>, modified: Option<SystemTime>) -> Result<()> {
        self.inner.finish(modified)
    }
}

impl Library for FaultyDrive {
    fn root_display(&self) -> String {
        self.inner.root_display()
    }
    fn space(&self) -> Result<LibrarySpace> {
        let mut s = self.inner.space()?;
        if let Some(free) = self.f.free_bytes {
            s.free_bytes = free;
        }
        Ok(s)
    }
    fn read_text(&self, r: &str) -> Option<String> {
        self.inner.read_text(r)
    }
    fn create_text(&self, r: &str, t: &str) -> Result<()> {
        self.inner.create_text(r, t)
    }
    fn begin(&self, d: &str, t: &str) -> Result<Box<dyn PartialWrite>> {
        let n = self.begins.fetch_add(1, Ordering::SeqCst) + 1;
        if let (1, Some((started, go))) = (n, &self.f.hold_first_begin) {
            started.send(()).unwrap();
            go.lock().unwrap().recv().unwrap();
        }
        if self.f.panic_in_begin {
            panic!("simulated bug in begin");
        }
        if self.f.begin_fails_from.is_some_and(|from| n >= from) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "drive gone").into());
        }
        let inner = self.inner.begin(d, t)?;
        Ok(Box::new(FaultyPartial { inner, written: self.written.clone(), limit: self.f.enospc_after }))
    }
    fn flush_to_media(&self, r: &str) -> Result<()> {
        if self.f.panic_after_naming && !r.ends_with(".partial") {
            panic!("simulated crash after naming");
        }
        self.inner.flush_to_media(r)
    }
    fn verify(&self, r: &str, size: u64, hash: u128) -> Result<()> {
        self.inner.verify(r, size, hash)
    }
    fn commit(&self, p: &str, d: &str) -> Result<Option<SystemTime>> {
        if self.f.squat_before_commit {
            fs::write(self.root.join(d), b"arrived meanwhile").unwrap();
        }
        self.inner.commit(p, d)
    }
    fn discard(&self, p: &str) -> Result<()> {
        self.inner.discard(p)
    }
}

// ---- a card that fails on cue -----------------------------------------------

struct FaultyCard {
    inner: FsSource,
    /// Reads of these files fail after 64 KiB.
    bad: HashSet<String>,
    /// Every streaming open fails (card pulled out after planning).
    pulled: bool,
}

struct FailingReader {
    inner: Box<dyn ReadSeek>,
    pos: u64,
}
impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= 64 * 1024 {
            return Err(io::Error::other("I/O error reading the card"));
        }
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}
impl Seek for FailingReader {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(p)?;
        Ok(self.pos)
    }
}

impl MediaSource for FaultyCard {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn list(&self) -> Result<Vec<MediaEntry>> {
        self.inner.list()
    }
    fn list_complete(&self) -> Result<Vec<MediaEntry>> {
        self.inner.list_complete()
    }
    fn open(&self, rel: &str) -> Result<Box<dyn ReadSeek>> {
        self.inner.open(rel)
    }
    fn open_streaming(&self, rel: &str) -> Result<Box<dyn ReadSeek>> {
        if self.pulled {
            return Err(io::Error::new(io::ErrorKind::NotFound, "card pulled out").into());
        }
        let r = self.inner.open_streaming(rel)?;
        Ok(if self.bad.contains(rel) { Box::new(FailingReader { inner: r, pos: 0 }) } else { r })
    }
}

// ---- helpers ----------------------------------------------------------------

fn plan_on(e: &Env, lib: Arc<dyn Library>) -> cull_core::offload::OffloadPlan {
    plan_with(e, lib)
}

/// Nothing half-written anywhere and every file on the drive is a complete copy of some card file.
fn assert_drive_consistent(e: &Env) {
    assert!(partials(&e.ssd).is_empty(), "temp files left: {:?}", partials(&e.ssd));
    let card: HashSet<Vec<u8>> = snapshot(&e.card).keys().map(|k| fs::read(e.card.join(k)).unwrap()).collect();
    for f in photo_files(&e.ssd) {
        if f.starts_with("Photos/") {
            assert!(card.contains(&fs::read(e.ssd.join(&f)).unwrap()), "{f} is not a complete copy of a card file");
        }
    }
}

/// Running again finishes the job and ends safe.
fn assert_rerun_finishes(e: &Env) {
    plan(e);
    let r = start(e);
    assert!(r.all_safe, "rerun: {r:?}");
    assert_drive_consistent(e);
}

// ---- 1-4: the card changes between planning and copying ----------------------

#[test]
fn e01_card_file_edited_after_planning_is_not_copied() {
    let e = env();
    plan(&e);
    let p = e.card.join("DCIM/100NZ6_2/DSC_0002.NEF");
    let mut b = fs::read(&p).unwrap();
    b[10] ^= 0xFF; // same size, different content
    fs::write(&p, &b).unwrap();
    let r = start(&e);
    assert!(!r.all_safe);
    assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
    assert!(r.failed[0].error.contains("changed since"), "{:?}", r.failed);
    assert!(!e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0002.NEF").exists());
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

#[test]
fn e02_card_file_that_grew_after_planning_is_not_copied() {
    let e = env();
    plan(&e);
    let p = e.card.join("DCIM/100_FUJI/DSCF0002.MOV");
    let mut b = fs::read(&p).unwrap();
    b.extend_from_slice(b"more");
    fs::write(&p, &b).unwrap();
    let r = start(&e);
    assert!(!r.all_safe && r.failed.len() == 1, "{r:?}");
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

#[test]
fn e03_card_file_deleted_after_planning_is_reported() {
    let e = env();
    plan(&e);
    fs::remove_file(e.card.join("DCIM/100_FUJI/DSCF0001.JPG")).unwrap();
    let r = start(&e);
    assert!(!r.all_safe, "a file that couldn't be copied is never 'all safe'");
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.copied, 4);
    assert_drive_consistent(&e);
}

#[test]
fn e04_another_card_at_the_same_path_copies_nothing_wrong() {
    let e = env();
    plan(&e);
    // Card swapped: same folders and names, different photos.
    fs::remove_dir_all(&e.card).unwrap();
    let f = e.card.join("DCIM/100_FUJI");
    put(&f.join("DSCF0001.JPG"), &jpeg("FUJIFILM", "2026:12:01 10:00:00", 99), None);
    put(&f.join("DSCF0001.RAF"), &raf("2026:12:01 10:00:00", 99), None);
    let r = start(&e);
    assert!(!r.all_safe);
    assert_eq!(r.copied, 0, "nothing from the other card goes in under the planned names");
    assert_eq!(r.not_backed_up.len(), 2, "{:?}", r.not_backed_up);
    assert!(photo_files(&e.ssd).iter().all(|p| !p.starts_with("Photos/Fuji") && !p.starts_with("Photos/Nikon")));
    assert!(partials(&e.ssd).is_empty());
}

// ---- 5-6: the card fails while copying ---------------------------------------

#[test]
fn e05_a_read_error_mid_file_fails_only_that_file() {
    let e = env();
    let card = FaultyCard { inner: FsSource::new(&e.card).unwrap(), bad: ["DCIM/100_FUJI/DSCF0002.MOV".to_string()].into(), pulled: false };
    try_plan(&e, Arc::new(card), fs_lib(&e), false).unwrap();
    let r = start(&e);
    assert!(!r.all_safe && r.error.is_none(), "{r:?}");
    assert_eq!((r.copied, r.failed.len()), (4, 1));
    assert!(!e.ssd.join("Photos/Fuji/2026/Japan trip/Videos/DSCF0002.MOV").exists());
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

#[test]
fn e06_a_card_pulled_out_stops_the_run_cleanly() {
    let e = env();
    let card = FaultyCard { inner: FsSource::new(&e.card).unwrap(), bad: HashSet::new(), pulled: true };
    try_plan(&e, Arc::new(card), fs_lib(&e), false).unwrap();
    let r = start(&e);
    assert!(r.error.as_deref().is_some_and(|m| m.contains("card")), "{r:?}");
    assert_eq!(r.copied, 0);
    assert!(r.failed.len() <= 3, "gives up after a few failures, not one per file: {:?}", r.failed);
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

// ---- 7-8: the drive fills up or disappears ---------------------------------

#[test]
fn e07_drive_full_mid_file_leaves_no_truncated_copy() {
    let e = env();
    // Fails in the middle of the 3rd file (JPEG ~ a few KB, RAF ~300 KB, MOV 500 KB…).
    plan_on(&e, drive(&e, Faults { enospc_after: Some(400_000), ..Default::default() }));
    let r = start(&e);
    assert!(r.error.as_deref().is_some_and(|m| m.contains("full")), "{r:?}");
    assert!(!r.all_safe);
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

#[test]
fn e08_drive_unplugged_mid_run_stops_and_keeps_verified_files() {
    let e = env();
    plan_on(&e, drive(&e, Faults { begin_fails_from: Some(3), ..Default::default() }));
    let r = start(&e);
    assert!(r.error.as_deref().is_some_and(|m| m.contains("drive")), "{r:?}");
    assert_eq!(r.copied, 2);
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

// ---- 9-10: names and folders taken at the last moment -------------------------

#[test]
fn e09_a_file_appearing_at_the_target_name_is_never_overwritten() {
    let e = env();
    plan_on(&e, drive(&e, Faults { squat_before_commit: true, ..Default::default() }));
    let r = start(&e);
    assert_eq!(r.copied, 0);
    assert_eq!(r.failed.len(), 5);
    for p in EXPECTED {
        assert_eq!(fs::read(e.ssd.join(p)).unwrap(), b"arrived meanwhile", "{p} was overwritten");
    }
    assert!(partials(&e.ssd).is_empty());
    // Next time they get other names.
    plan(&e);
    assert!(start(&e).all_safe);
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/DSC_0001-1.JPG").exists());
}

#[test]
fn e10_a_file_where_a_folder_should_be_fails_cleanly() {
    let e = env();
    put(&e.ssd.join("Photos/Fuji"), b"not a folder", None);
    plan(&e);
    let r = start(&e);
    assert!(!r.all_safe);
    assert_eq!(fs::read(e.ssd.join("Photos/Fuji")).unwrap(), b"not a folder");
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/DSC_0001.JPG").exists() || r.error.is_some(), "{r:?}");
    assert!(partials(&e.ssd).is_empty());
}

// ---- 11-13: odd files on the card -------------------------------------------

#[test]
fn e11_the_same_photo_twice_on_a_card_is_backed_up() {
    let e = env();
    let jpg = fs::read(e.card.join("DCIM/100_FUJI/DSCF0001.JPG")).unwrap();
    put(&e.card.join("DCIM/101_FUJI/DSCF0001.JPG"), &jpg, None);
    plan(&e);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_drive_consistent(&e);
}

#[test]
fn e12_an_empty_file_on_the_card_is_copied_and_checked() {
    let e = env();
    put(&e.card.join("DCIM/100_FUJI/DSCF0009.JPG"), b"", Some("2026-10-07T10:00:00"));
    plan(&e);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_eq!(fs::metadata(e.ssd.join("Photos/Fuji/2026/Japan trip/DSCF0009.JPG")).unwrap().len(), 0);
}

#[test]
fn e13_an_unreadable_folder_on_the_card_is_never_called_safe() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    let locked = e.card.join("DCIM/102_FUJI");
    put(&locked.join("DSCF0100.JPG"), &jpeg("FUJIFILM", "2026:10:08 10:00:00", 50), None);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let res = try_plan(&e, Arc::new(FsSource::new(&e.card).unwrap()), fs_lib(&e), false);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let err = res.expect_err("a card folder that can't be read must stop the plan, not be skipped");
    assert!(err.to_string().contains("102_FUJI"), "{err}");
}

#[test]
fn e14_hidden_macos_files_on_the_card_are_ignored() {
    let e = env();
    put(&e.card.join("DCIM/100_FUJI/._DSCF0001.JPG"), b"AppleDouble", None);
    put(&e.card.join(".Spotlight-V100/store.db"), b"x", None);
    let p = plan(&e);
    assert_eq!(p.new_files, 5);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
}

// ---- 15-17: names from tags and choices -------------------------------------

#[test]
fn e15_tags_differing_only_in_case_share_one_folder() {
    let e = env();
    // The saved tag "Japan trip" (Oct 1–14) and an all-day calendar event "JAPAN TRIP" in November.
    struct Cal;
    impl cull_core::CalendarSource for Cal {
        fn events(&self, _: &str, _: &str) -> Result<Vec<cull_core::CalendarEvent>> {
            Ok(vec![cull_core::CalendarEvent { title: "JAPAN TRIP".into(), start_day: "2026-11-01".into(), end_day: "2026-11-03".into(), all_day: true }])
        }
    }
    e.svc.set_calendar(Some(Arc::new(Cal)));
    // A Fuji photo in each: same brand, same year, same trip name in two spellings.
    put(&e.card.join("DCIM/100_FUJI/DSCF0300.JPG"), &jpeg("FUJIFILM", "2026:11:02 12:00:00", 70), None);
    let p = plan(&e);
    let fuji: Vec<&str> = p.folders.iter().filter(|f| f.brand == "Fuji").map(|f| f.path.as_str()).collect();
    assert_eq!(fuji, ["Photos/Fuji/2026/Japan trip"], "one folder, first spelling");
    let ids: Vec<&str> = p.folders.iter().map(|f| f.id.as_str()).collect();
    let unique: HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "duplicate folder ids: {ids:?}");
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
}

#[test]
fn e16_folder_names_cannot_escape_the_drive_folder() {
    for evil in ["../../evil", "/etc", ".", "..", "_darkroom/../../x", "a/b", "\u{0}nul", " ", ".hidden"] {
        let e = env();
        plan(&e);
        let id = "Fuji|2026|trip:japan trip".to_string();
        let r = e.svc.offload_start(&[FolderChoice { id, name: Some(evil.into()), by_month: false }], &NullSink).unwrap();
        assert!(r.all_safe, "{evil:?}: {r:?}");
        let outside: Vec<String> = snapshot(e.ssd.parent().unwrap())
            .into_keys()
            .filter(|k| !k.starts_with("ssd/") && !k.starts_with("card/") && !k.starts_with("cache"))
            .collect();
        assert!(outside.is_empty(), "{evil:?} wrote outside the drive folder: {outside:?}");
        for f in photo_files(&e.ssd) {
            assert!(f.starts_with("Photos/Fuji/2026/") || f.starts_with("Photos/Nikon/2026/"), "{evil:?} → {f}");
            assert!(!f.contains("/."), "{evil:?} made a hidden path: {f}");
        }
    }
}

#[test]
fn e17_dates_at_the_edges() {
    let e = env();
    // 3 a.m. on New Year's Day belongs to Dec 31 of the year before.
    put(&e.card.join("DCIM/100_FUJI/DSCF0200.JPG"), &jpeg("FUJIFILM", "2027:01:01 03:00:00", 60), None);
    // A broken date: no capture time from EXIF, mtime used.
    put(&e.card.join("DCIM/100_FUJI/DSCF0201.JPG"), &jpeg("FUJIFILM", "0000:00:00 00:00:00", 61), Some("2026-12-24T12:00:00"));
    plan(&e);
    assert!(start(&e).all_safe);
    assert!(e.ssd.join("Photos/Fuji/2026/2026-12/DSCF0200.JPG").exists());
    assert!(e.ssd.join("Photos/Fuji/2026/2026-12/DSCF0201.JPG").exists());
}

// ---- 18-21: crashes, concurrency, space, a broken id file --------------------

#[test]
fn e18_a_bug_mid_run_never_locks_backups_forever() {
    let e = env();
    plan_on(&e, drive(&e, Faults { panic_in_begin: true, ..Default::default() }));
    let svc = e.svc.clone();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| svc.offload_start(&[], &NullSink)));
    assert!(matches!(res, Ok(Err(_))), "a panic inside the run comes back as an error");
    assert_drive_consistent(&e);
    assert_rerun_finishes(&e);
}

#[test]
fn e19_named_on_the_drive_but_never_recorded_is_found_next_time() {
    let e = env();
    plan_on(&e, drive(&e, Faults { panic_after_naming: true, ..Default::default() }));
    let r = start(&e);
    assert!(!r.all_safe && r.copied == 0, "{r:?}");
    // The files are there under their final names (complete), just not recorded.
    let named = photo_files(&e.ssd).into_iter().filter(|p| p.starts_with("Photos/Fuji") || p.starts_with("Photos/Nikon")).count();
    assert_eq!(named, 5);
    assert_drive_consistent(&e);
    // Next time they count as already there: no second copies.
    let p = plan(&e);
    assert_eq!((p.new_files, p.already_files), (0, 5));
    assert!(start(&e).all_safe);
}

#[test]
fn e20_two_runs_at_once_are_refused() {
    let e = env();
    let (started_tx, started_rx) = sync_channel(1);
    let (go_tx, go_rx) = sync_channel(1);
    plan_on(&e, drive(&e, Faults { hold_first_begin: Some((started_tx, Mutex::new(go_rx))), ..Default::default() }));
    let svc = e.svc.clone();
    let first = std::thread::spawn(move || svc.offload_start(&[], &NullSink));
    started_rx.recv().unwrap();
    let second = e.svc.offload_start(&[], &NullSink);
    assert!(second.is_err_and(|e| e.to_string().contains("already running")));
    assert!(try_plan(&e, Arc::new(FsSource::new(&e.card).unwrap()), fs_lib(&e), false).is_err());
    go_tx.send(()).unwrap();
    assert!(first.join().unwrap().unwrap().all_safe);
}

#[test]
fn e21_nothing_to_copy_needs_no_free_space() {
    let e = env();
    plan(&e);
    assert!(start(&e).all_safe);
    let p = plan_on(&e, drive(&e, Faults { free_bytes: Some(10 << 20), ..Default::default() }));
    assert_eq!(p.new_files, 0);
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    assert!(start(&e).all_safe);
}

#[test]
fn e22_too_little_space_is_refused_before_anything_is_written() {
    let e = env();
    let p = plan_on(&e, drive(&e, Faults { free_bytes: Some(10 << 20), ..Default::default() }));
    assert!(p.warnings.iter().any(|w| w.contains("space")), "{:?}", p.warnings);
    assert!(e.svc.offload_start(&[], &NullSink).is_err());
    assert!(fs::read_dir(&e.ssd).unwrap().next().is_none(), "nothing written");
}

#[test]
fn e23_a_broken_library_id_stops_cleanly() {
    let e = env();
    fs::create_dir_all(e.ssd.join("Photos/_darkroom/library-id")).unwrap(); // a folder, not a file
    plan(&e);
    let r = e.svc.offload_start(&[], &NullSink);
    assert!(r.is_err(), "{r:?}");
    assert!(photo_files(&e.ssd).iter().all(|p| p.starts_with("Photos/_darkroom")), "nothing copied");
}


// ---- 24-27: writes stay on the drive, inside the picked folder --------------

#[test]
fn e24_a_symlink_on_the_drive_cannot_redirect_writes_onto_the_card() {
    let e = env();
    let card_before = snapshot(&e.card);
    // "Photos" on the drive is a symlink into the card.
    std::os::unix::fs::symlink(e.card.join("DCIM"), e.ssd.join("Photos")).unwrap();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        plan(&e);
        e.svc.offload_start(&[], &NullSink)
    }));
    let report = res.expect("no panic");
    assert!(report.is_err() || !report.as_ref().unwrap().all_safe, "{report:?}");
    assert_eq!(snapshot(&e.card), card_before, "nothing written onto the card through the symlink");
}

#[test]
fn e25_a_symlinked_subfolder_pointing_elsewhere_is_refused() {
    let e = env();
    let elsewhere = e.ssd.parent().unwrap().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(e.ssd.join("Photos")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, e.ssd.join("Photos/Fuji")).unwrap();
    plan(&e);
    let r = start(&e);
    assert!(!r.all_safe);
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written outside the drive folder");
    // The Nikon files (no symlink on their way) still go in.
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/DSC_0001.JPG").exists());
    assert!(partials(&e.ssd).is_empty());
}

#[test]
fn e26_a_symlink_that_stays_inside_the_drive_folder_is_fine() {
    let e = env();
    fs::create_dir_all(e.ssd.join("Archive/Fuji")).unwrap();
    fs::create_dir_all(e.ssd.join("Photos")).unwrap();
    std::os::unix::fs::symlink(e.ssd.join("Archive/Fuji"), e.ssd.join("Photos/Fuji")).unwrap();
    plan(&e);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert!(e.ssd.join("Archive/Fuji/2026/Japan trip/DSCF0001.JPG").exists());
}

#[test]
fn e27_picking_the_card_as_the_drive_is_refused() {
    let e = env();
    // The "drive" has DCIM (a card); the "card" doesn't (the SSD): swapped.
    fs::create_dir_all(e.ssd.join("DCIM/100NIKON")).unwrap();
    let swapped = FsLibrary::allowing_same_volume_for_tests(&e.ssd, e.card.join("DCIM/100_FUJI"));
    assert!(swapped.is_ok(), "the card side has DCIM above it: not swapped");
    let ssd_as_card = e.ssd.parent().unwrap().join("ssd-as-card");
    fs::create_dir_all(&ssd_as_card).unwrap();
    let err = FsLibrary::allowing_same_volume_for_tests(&e.ssd, &ssd_as_card).err().expect("swapped picks are refused");
    assert!(err.to_string().contains("other way round"), "{err}");
    // Both look like cards: allowed, with a warning in the plan.
    let p = plan(&e);
    assert!(p.warnings.iter().any(|w| w.contains("DCIM")), "{:?}", p.warnings);
}
