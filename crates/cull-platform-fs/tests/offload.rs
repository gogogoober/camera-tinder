//! Offload (card → backup drive) on real files: layout, matching by content, verification, stops,
//! corruption, never overwriting, and the card left untouched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cull_core::offload::FolderChoice;
use cull_core::{CullerService, Library, LibrarySpace, NullSink, PartialWrite, Result};
use cull_platform_fs::{FsLibrary, FsSource};

mod common;
use common::*;

// ---- tests -----------------------------------------------------------------

#[test]
fn backs_up_every_photo_and_video_into_trip_and_month_folders() {
    let e = env();
    let card_before = snapshot(&e.card);

    let p = plan(&e);
    assert_eq!((p.new_files, p.already_files), (5, 0));
    assert_eq!(p.skipped.iter().map(|s| (s.extension.as_str(), s.count)).collect::<Vec<_>>(), [("LRF", 1)]);
    let names: Vec<_> = p.folders.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["Photos/Fuji/2026/Japan trip", "Photos/Nikon/2026/2026-11"]);
    assert!(fs::read_dir(&e.ssd).unwrap().next().is_none(), "planning writes nothing");

    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_eq!((r.copied, r.already), (5, 0));
    // Each step's bytes and time are reported.
    let s = &r.stages;
    assert_eq!((s.read_bytes, s.write_bytes, s.check_bytes), (r.bytes, r.bytes, r.bytes));
    assert!(s.read_seconds > 0.0 && s.write_seconds > 0.0 && s.check_seconds > 0.0, "{s:?}");
    let mut got = photo_files(&e.ssd);
    got.sort();
    let mut want: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
    // Bit for bit.
    let pairs = [
        ("DCIM/100_FUJI/DSCF0001.RAF", "Photos/Fuji/2026/Japan trip/RAW/DSCF0001.RAF"),
        ("DCIM/100_FUJI/DSCF0002.MOV", "Photos/Fuji/2026/Japan trip/Videos/DSCF0002.MOV"),
        ("DCIM/100NZ6_2/DSC_0001.JPG", "Photos/Nikon/2026/2026-11/DSC_0001.JPG"),
    ];
    for (a, b) in pairs {
        assert_eq!(fs::read(e.card.join(a)).unwrap(), fs::read(e.ssd.join(b)).unwrap(), "{b}");
    }
    // Hash list and library id, nothing else of ours.
    let manifest = fs::read_to_string(e.ssd.join(r.manifest.as_ref().unwrap())).unwrap();
    assert_eq!(manifest.lines().filter(|l| !l.starts_with('#')).count(), 5);
    assert!(manifest.contains("Fuji/2026/Japan trip/RAW/DSCF0001.RAF"));
    assert!(e.ssd.join("Photos/_darkroom/library-id").is_file());
    assert!(partials(&e.ssd).is_empty());
    assert_eq!(snapshot(&e.card), card_before, "the card is never changed");

    // Again: everything is already there.
    let p = plan(&e);
    assert_eq!((p.new_files, p.already_files), (0, 5));
    let r = start(&e);
    assert!(r.all_safe && r.copied == 0 && r.already == 5);
    assert_eq!(p.last_backups.iter().map(|b| b.brand.as_str()).collect::<Vec<_>>(), ["Fuji", "Nikon"]);
}

/// Planning reads only the start of a RAW file for its camera and date, not the whole file.
#[test]
fn planning_reads_only_the_start_of_raw_files() {
    let e = env();
    fs::remove_dir_all(&e.card).unwrap();
    for i in 0..5u8 {
        put(&e.card.join(format!("DCIM/100NIKON/DSC_{i:04}.NEF")), &nef("2026:11:03 15:00:00", i, 8 << 20), None);
    }
    // No EXIF at all: still works, by reading the whole file (as before).
    put(&e.card.join("DCIM/100NIKON/DSC_0099.NEF"), &noise(3 << 20, 9), Some("2026-11-04T10:00:00"));
    let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let card = CountingSource { inner: FsSource::new(&e.card).unwrap(), bytes: bytes.clone() };
    let p = try_plan(&e, Arc::new(card), fs_lib(&e), false).unwrap();
    let read = bytes.load(std::sync::atomic::Ordering::Relaxed);
    assert!(read < 5 * (1 << 20) + (4 << 20), "planning read {} MB of a 43 MB card", read >> 20);
    assert_eq!(p.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["Photos/Nikon/2026/2026-11"]);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0000.NEF").exists());
}

#[test]
fn already_backed_up_means_same_content_anywhere_on_the_drive() {
    let e = env();
    // An old manual backup: other folders, other names.
    put(&e.ssd.join("2026 stuff/japan/a.jpg"), &fs::read(e.card.join("DCIM/100_FUJI/DSCF0001.JPG")).unwrap(), None);
    put(&e.ssd.join("misc/clip.mov"), &fs::read(e.card.join("DCIM/100_FUJI/DSCF0002.MOV")).unwrap(), None);
    // Same name and size but different first bytes: not a backup.
    let mut other = fs::read(e.card.join("DCIM/100NZ6_2/DSC_0002.NEF")).unwrap();
    other[100] ^= 0xFF;
    put(&e.ssd.join("misc/DSC_0002.NEF"), &other, None);

    let p = plan(&e);
    assert_eq!((p.new_files, p.already_files), (3, 2));
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert!(!e.ssd.join("Photos/Fuji/2026/Japan trip/DSCF0001.JPG").exists());
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0002.NEF").exists());
}

/// The quick match (size + first 64 KiB + last 16 KiB) can't see damage in the middle of the drive's
/// copy; the full check can.
#[test]
fn the_full_check_catches_a_drive_copy_damaged_in_the_middle() {
    let e = env();
    let mut rotten = fs::read(e.card.join("DCIM/100NZ6_2/DSC_0002.NEF")).unwrap();
    rotten[150_000] ^= 0xFF;
    put(&e.ssd.join("old/DSC_0002.NEF"), &rotten, None);
    put(&e.ssd.join("old/a.jpg"), &fs::read(e.card.join("DCIM/100_FUJI/DSCF0001.JPG")).unwrap(), None);

    let quick = plan(&e);
    assert_eq!(quick.already_files, 2, "quick match trusts the damaged copy");
    let full = plan_full(&e, fs_lib(&e), true);
    assert!(full.full_check);
    assert_eq!((full.new_files, full.already_files), (4, 1));
    assert!(start(&e).all_safe);
    assert_eq!(fs::read(e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0002.NEF")).unwrap(), fs::read(e.card.join("DCIM/100NZ6_2/DSC_0002.NEF")).unwrap());
}

#[test]
fn never_overwrites_a_file_already_at_the_target_name() {
    let e = env();
    let squatter = e.ssd.join("Photos/Nikon/2026/2026-11/DSC_0001.JPG");
    put(&squatter, b"someone else's photo", None);
    let p = plan(&e);
    assert_eq!(p.new_files, 5);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_eq!(fs::read(&squatter).unwrap(), b"someone else's photo");
    assert!(e.ssd.join("Photos/Nikon/2026/2026-11/DSC_0001-1.JPG").exists());
}

#[test]
fn a_trip_folder_renamed_in_finder_is_found_again() {
    let e = env();
    plan(&e);
    assert!(start(&e).all_safe);
    fs::rename(e.ssd.join("Photos/Fuji/2026/Japan trip"), e.ssd.join("Photos/Fuji/2026/Japan 2026 🇯🇵")).unwrap();
    put(&e.card.join("DCIM/100_FUJI/DSCF0003.JPG"), &jpeg("FUJIFILM", "2026:10:09 09:00:00", 9), None);
    let p = plan(&e);
    assert_eq!(p.new_files, 1);
    assert_eq!(p.folders[0].path, "Photos/Fuji/2026/Japan 2026 🇯🇵");
    assert!(start(&e).all_safe);
    assert!(e.ssd.join("Photos/Fuji/2026/Japan 2026 🇯🇵/DSCF0003.JPG").exists());
}

#[test]
fn folder_choices_rename_and_split_by_month() {
    let e = env();
    plan(&e);
    let id = "Fuji|2026|trip:japan trip".to_string();
    let p = e.svc.offload_layout(&[FolderChoice { id: id.clone(), name: Some("Japan".into()), by_month: false }]).unwrap();
    assert!(p.folders.iter().any(|f| f.path == "Photos/Fuji/2026/Japan"));
    let p = e.svc.offload_layout(&[FolderChoice { id: id.clone(), name: None, by_month: true }]).unwrap();
    assert!(p.folders.iter().any(|f| f.path == "Photos/Fuji/2026/2026-10"));
    let r = e.svc.offload_start(&[FolderChoice { id, name: Some("Japan".into()), by_month: false }], &NullSink).unwrap();
    assert!(r.all_safe);
    assert!(e.ssd.join("Photos/Fuji/2026/Japan/RAW/DSCF0001.RAF").exists());
}

/// Corrupts every temp file just before it is read back, as a failing drive would.
struct CorruptingLibrary(FsLibrary, PathBuf);
impl Library for CorruptingLibrary {
    fn root_display(&self) -> String {
        self.0.root_display()
    }
    fn space(&self) -> Result<LibrarySpace> {
        self.0.space()
    }
    fn read_text(&self, r: &str) -> Option<String> {
        self.0.read_text(r)
    }
    fn create_text(&self, r: &str, t: &str) -> Result<()> {
        self.0.create_text(r, t)
    }
    fn begin(&self, d: &str, t: &str) -> Result<Box<dyn PartialWrite>> {
        self.0.begin(d, t)
    }
    fn flush_to_media(&self, r: &str) -> Result<()> {
        self.0.flush_to_media(r)
    }
    fn verify(&self, r: &str, size: u64, hash: u128) -> Result<()> {
        if r.ends_with(".partial") && r.contains("DSC_0002.NEF") {
            let path = self.1.join(r);
            let mut bytes = fs::read(&path).unwrap();
            bytes[1000] ^= 1;
            fs::write(&path, bytes).unwrap();
        }
        self.0.verify(r, size, hash)
    }
    fn commit(&self, p: &str, d: &str) -> Result<Option<std::time::SystemTime>> {
        self.0.commit(p, d)
    }
    fn discard(&self, p: &str) -> Result<()> {
        self.0.discard(p)
    }
}

#[test]
fn a_copy_that_reads_back_wrong_is_never_counted_or_named() {
    let e = env();
    let lib = CorruptingLibrary(FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap(), fs::canonicalize(&e.ssd).unwrap());
    plan_with(&e, Arc::new(lib));
    let r = start(&e);
    assert!(!r.all_safe);
    assert_eq!(r.copied, 4);
    assert_eq!(r.failed.len(), 1);
    assert!(r.failed[0].error.contains("doesn't match"), "{:?}", r.failed);
    assert_eq!(r.not_backed_up, ["DCIM/100NZ6_2/DSC_0002.NEF"]);
    assert!(!e.ssd.join("Photos/Nikon/2026/2026-11/RAW/DSC_0002.NEF").exists());
    assert!(partials(&e.ssd).is_empty());
    // The next run copies it.
    assert_eq!(plan(&e).new_files, 1);
    assert!(start(&e).all_safe);
}

/// Stops the run when the copy of the `n`th file starts.
struct StopAtBegin(FsLibrary, Arc<CullerService>, std::sync::atomic::AtomicUsize, usize);
impl Library for StopAtBegin {
    fn root_display(&self) -> String {
        self.0.root_display()
    }
    fn space(&self) -> Result<LibrarySpace> {
        self.0.space()
    }
    fn read_text(&self, r: &str) -> Option<String> {
        self.0.read_text(r)
    }
    fn create_text(&self, r: &str, t: &str) -> Result<()> {
        self.0.create_text(r, t)
    }
    fn begin(&self, d: &str, t: &str) -> Result<Box<dyn PartialWrite>> {
        if self.2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.3 {
            self.1.offload_stop();
        }
        self.0.begin(d, t)
    }
    fn flush_to_media(&self, r: &str) -> Result<()> {
        self.0.flush_to_media(r)
    }
    fn verify(&self, r: &str, size: u64, hash: u128) -> Result<()> {
        self.0.verify(r, size, hash)
    }
    fn commit(&self, p: &str, d: &str) -> Result<Option<std::time::SystemTime>> {
        self.0.commit(p, d)
    }
    fn discard(&self, p: &str) -> Result<()> {
        self.0.discard(p)
    }
}

#[test]
fn stopping_mid_copy_leaves_only_complete_verified_files() {
    let e = env();
    // Big enough files that the stop lands mid-file.
    for i in 0..4u8 {
        put(&e.card.join(format!("DCIM/100_FUJI/DSCF01{i:02}.MOV")), &noise(12 << 20, 10 + i), Some("2026-10-06T10:00:00"));
    }
    let lib = StopAtBegin(FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap(), e.svc.clone(), Default::default(), 4);
    plan_with(&e, Arc::new(lib));
    let r = e.svc.offload_start(&[], &NullSink).unwrap();
    assert!(r.stopped && !r.all_safe);
    assert!(r.copied >= 3 && r.copied < 9, "{}", r.copied);
    assert!(partials(&e.ssd).is_empty());
    // Everything on the drive is a complete copy of a card file.
    let card: Vec<Vec<u8>> = snapshot(&e.card).keys().map(|k| fs::read(e.card.join(k)).unwrap()).collect();
    for f in photo_files(&e.ssd) {
        assert!(card.contains(&fs::read(e.ssd.join(&f)).unwrap()), "{f} is not a complete copy");
    }
    // Running again finishes the job.
    plan(&e);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_eq!(photo_files(&e.ssd).len(), 9);
}

/// Crashes (panics) the first time a copy is read back, like the app dying mid-run.
struct CrashOnVerify(FsLibrary);
impl Library for CrashOnVerify {
    fn root_display(&self) -> String {
        self.0.root_display()
    }
    fn space(&self) -> Result<LibrarySpace> {
        self.0.space()
    }
    fn read_text(&self, r: &str) -> Option<String> {
        self.0.read_text(r)
    }
    fn create_text(&self, r: &str, t: &str) -> Result<()> {
        self.0.create_text(r, t)
    }
    fn begin(&self, d: &str, t: &str) -> Result<Box<dyn PartialWrite>> {
        self.0.begin(d, t)
    }
    fn flush_to_media(&self, r: &str) -> Result<()> {
        self.0.flush_to_media(r)
    }
    fn verify(&self, _: &str, _: u64, _: u128) -> Result<()> {
        panic!("simulated crash during read-back");
    }
    fn commit(&self, p: &str, d: &str) -> Result<Option<std::time::SystemTime>> {
        self.0.commit(p, d)
    }
    fn discard(&self, p: &str) -> Result<()> {
        self.0.discard(p)
    }
}

#[test]
fn temp_files_left_by_a_crash_are_removed_by_the_next_run() {
    let e = env();
    // A file of the user's with a temp-like name is never touched.
    put(&e.ssd.join("Photos/.notes.partial"), b"mine", None);
    plan_with(&e, Arc::new(CrashOnVerify(FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap())));
    let r = start(&e);
    assert!(r.error.is_some() && r.copied == 0 && !r.all_safe, "{r:?}");
    let left = partials(&e.ssd);
    assert!(left.iter().any(|p| p.contains(".darkroom-")), "the crash left temp files: {left:?}");

    plan(&e);
    let r = start(&e);
    assert!(r.all_safe, "{r:?}");
    assert_eq!(partials(&e.ssd), ["Photos/.notes.partial"]);
    assert_eq!(fs::read(e.ssd.join("Photos/.notes.partial")).unwrap(), b"mine");
}

#[test]
fn the_drive_refuses_unsafe_operations() {
    let e = env();
    // Same volume as the card (temp dirs share a disk): refused by the real constructor.
    assert!(FsLibrary::new(&e.ssd, &e.card).is_err());
    // One inside the other: refused even for tests.
    assert!(FsLibrary::allowing_same_volume_for_tests(e.card.join("DCIM"), &e.card).is_err());
    let lib = FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap();
    put(&e.ssd.join("keep.jpg"), b"keep", None);
    assert!(lib.discard("keep.jpg").is_err());
    assert!(lib.discard("../card/DCIM/100_FUJI/DSCF0001.JPG").is_err());
    assert!(lib.begin("../escape.jpg", "x").is_err());
    let mut w = lib.begin("a/b.jpg", "t1").unwrap();
    w.write(b"hello").unwrap();
    let rel = w.rel_path().to_string();
    w.finish().unwrap();
    assert!(lib.commit(&rel, "keep.jpg").is_err(), "never overwrites");
    assert_eq!(fs::read(e.ssd.join("keep.jpg")).unwrap(), b"keep");
    assert!(lib.commit("keep.jpg", "other.jpg").is_err(), "only our temp files are committed");
    assert!(lib.create_text("keep.jpg", "x").is_err());
    lib.discard(&rel).unwrap();
    assert!(!e.ssd.join(&rel).exists());
}

/// Pages of `path` held in memory (macOS `mincore`): (resident, total).
#[cfg(target_vendor = "apple")]
fn resident_pages(path: &Path) -> (usize, usize) {
    use std::os::fd::AsRawFd;
    let f = fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: read-only shared mapping of a file we just opened, unmapped before returning.
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, f.as_raw_fd(), 0);
        assert!(p != libc::MAP_FAILED);
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let mut v = vec![0i8; len.div_ceil(page)];
        assert_eq!(libc::mincore(p, len, v.as_mut_ptr() as *mut _), 0);
        libc::munmap(p, len);
        (v.iter().filter(|b| **b & 1 != 0).count(), v.len())
    }
}

/// The read-back really reads the drive. An uncached read is still answered from memory when the
/// file's pages are there (measured: a normal write then an `F_NOCACHE` read runs at memory speed),
/// so the engine also writes uncached: not one page of the copy is in memory when it's read back.
#[test]
#[cfg(target_vendor = "apple")]
fn verification_reads_from_the_drive_not_from_memory() {
    let e = env();
    let lib = FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap();
    let data = noise(32 << 20, 77);

    // The probe works: a normally written file sits in memory.
    let normal = e.ssd.join("normal.bin");
    fs::write(&normal, &data).unwrap();
    let (r, n) = resident_pages(&normal);
    assert!(r > n / 2, "probe sees cached pages ({r}/{n})");

    let mut w = lib.begin("big.mov", "readback").unwrap();
    for c in data.chunks(4 << 20) {
        w.write(c).unwrap();
    }
    let rel = w.rel_path().to_string();
    w.finish().unwrap();
    lib.flush_to_media(&rel).unwrap();
    assert_eq!(resident_pages(&e.ssd.join(&rel)).0, 0, "the copy must not be in memory before it is read back");
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(&data);
    lib.verify(&rel, data.len() as u64, h.digest128()).unwrap();
    assert_eq!(resident_pages(&e.ssd.join(&rel)).0, 0, "the read-back doesn't fill memory either");
    lib.discard(&rel).unwrap();
}

/// A mounted disk image, ejected when dropped.
struct Volume {
    mount: String,
    image: PathBuf,
}
impl Volume {
    /// `sparse`: the image only takes the space it uses (for drives bigger than the 1 GB margin).
    fn mount(dir: &Path, label: &str, fs_kind: &str, size: &str, sparse: bool) -> Volume {
        use std::process::Command;
        let image = dir.join(if sparse { format!("{label}.sparseimage") } else { format!("{label}.dmg") });
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("O{}{}{n}", &label[..1], std::process::id() % 100_000);
        let mut create = Command::new("hdiutil");
        create.args(["create", "-quiet", "-size", size, "-fs", fs_kind, "-volname", &name]);
        if sparse {
            create.args(["-type", "SPARSE"]);
        }
        assert!(create.arg(&image).status().unwrap().success());
        assert!(Command::new("hdiutil").args(["attach", "-quiet", "-nobrowse"]).arg(&image).status().unwrap().success());
        Volume { mount: format!("/Volumes/{name}"), image }
    }
    fn path(&self) -> &Path {
        Path::new(&self.mount)
    }
    fn eject(&self) {
        assert!(std::process::Command::new("hdiutil").args(["detach", "-quiet", &self.mount]).status().unwrap().success());
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        if Path::new(&self.mount).exists() {
            let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet", "-force", &self.mount]).status();
        }
    }
}

/// Real volumes: an exFAT card offloaded to an exFAT drive (no hard links: the claim + rename path),
/// then to an APFS drive. Every raw byte of the card is unchanged afterwards.
/// `cargo test -p cull-platform-fs --test offload -- --ignored`
#[test]
#[ignore]
fn real_volumes_card_untouched_and_exfat_drive_works() {
    let tmp = tempfile::tempdir().unwrap();
    let card = Volume::mount(tmp.path(), "card", "ExFAT", "40m", false);
    make_card(card.path());
    let card_hash = |v: &Volume| xxhash_rust::xxh3::xxh3_64(&fs::read(&v.image).unwrap());
    card.eject();
    let before = card_hash(&card);
    assert!(std::process::Command::new("hdiutil").args(["attach", "-quiet", "-nobrowse", "-readonly"]).arg(&card.image).status().unwrap().success());

    for fs_kind in ["ExFAT", "APFS"] {
        let drive = Volume::mount(tmp.path(), &format!("d{fs_kind}"), fs_kind, "2g", true);
        assert!(FsLibrary::new(card.path(), card.path()).is_err(), "never the card itself");
        let svc = Arc::new(CullerService::in_memory(&tmp.path().join(format!("cache-{fs_kind}"))).unwrap());
        let plan = svc
            .offload_plan(
                Arc::new(FsSource::new(card.path()).unwrap()),
                card.mount.clone(),
                Arc::new(FsSource::new(drive.path()).unwrap()),
                Arc::new(FsLibrary::new(drive.path(), card.path()).unwrap()),
                true,
                &NullSink,
            )
            .unwrap();
        assert_eq!(plan.new_files, 5);
        assert_eq!(plan.warnings.is_empty(), fs_kind == "APFS", "{:?}", plan.warnings);
        let r = svc.offload_start(&[], &NullSink).unwrap();
        assert!(r.all_safe, "{fs_kind}: {r:?}");
        assert!(partials(drive.path()).is_empty());
        assert_eq!(photo_files(drive.path()).iter().filter(|p| !p.starts_with("._") && !p.contains("/._")).count(), 5);
    }
    card.eject();
    assert_eq!(card_hash(&card), before, "every raw byte of the card is unchanged");
}
