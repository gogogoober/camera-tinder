//! Failure scenarios found in the Nov 2026 originals-safety audit: card swaps, clearing previews
//! mid-scan, photos moved to another folder, a crashing file, renamed files, letter-case paths.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use cull_core::*;
use cull_core::scan;
use cull_platform_fs::{CopyToDir, FsSource};
use image::{GenericImageView, ImageBuffer, Rgb};

fn write_jpeg(path: &Path, w: u32, h: u32, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    ImageBuffer::from_fn(w, h, |x, y| Rgb([shade, (x % 256) as u8, (y % 256) as u8])).save(path).unwrap();
}

fn card(root: &Path, n: u8) {
    for i in 0..n {
        write_jpeg(&root.join(format!("DCIM/DSC_{i:04}.JPG")), 320, 240, i.wrapping_mul(5));
    }
}

/// Card swap: the old photo's previews are gone and a different photo now sits at its path.
/// Viewing the old photo must never cache the new photo's image under the old photo's identity.
#[test]
fn a_preview_is_never_made_from_a_different_file_at_the_same_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    write_jpeg(&root.join("DCIM/DSC_0001.JPG"), 1600, 1200, 10);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let old = svc.queue(&Filter::default()).unwrap().remove(0);

    svc.clear_previews(&|_| false).unwrap();
    write_jpeg(&root.join("DCIM/DSC_0001.JPG"), 800, 600, 200); // a different photo, same name

    match svc.preview(old.id, 2048) {
        Err(_) => {}
        Ok(bytes) => assert_ne!(image::load_from_memory(&bytes).unwrap().dimensions(), (800, 600), "old photo shows the new photo's image"),
    }
}

/// Presses "Clear previews" as soon as the first batch is saved (scan of an internal folder,
/// so only the running scan can stop it).
struct ClearDuringScan {
    svc: OnceLock<Arc<CullerService>>,
    refused: Mutex<Option<bool>>,
}
impl EventSink for ClearDuringScan {
    fn emit(&self, event: CoreEvent) {
        if let CoreEvent::ScanSaved { .. } = event {
            let mut refused = self.refused.lock().unwrap();
            if refused.is_none() {
                *refused = Some(self.svc.get().unwrap().clear_previews(&|_| false).is_err());
            }
        }
    }
}

#[test]
fn clearing_previews_during_a_scan_never_leaves_photos_without_previews() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 40);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    let src = svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    let sink = ClearDuringScan { svc: OnceLock::new(), refused: Mutex::new(None) };
    sink.svc.set(svc.clone()).ok();
    svc.scan(&Default::default(), &sink).unwrap();
    assert_eq!(*sink.refused.lock().unwrap(), Some(true), "clearing is refused while a scan runs");
    assert!(svc.clear_previews(&|_| false).is_ok(), "and allowed again once it's done");
    svc.scan(&Default::default(), &NullSink).unwrap(); // the user's next plain rescan rebuilds them

    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), 40);
    svc.remove_source(&src.id).unwrap(); // card unplugged
    let missing = ids.iter().filter(|id| svc.preview(**id, 256).is_err()).count();
    assert_eq!(missing, 0, "photos that can't be viewed once the card is unplugged");
}

/// Card scanned, then a copy of it on the laptop scanned (photos move to the copy), then the copy is
/// removed from the session. Rescanning the card must list its photos again.
#[test]
fn photos_come_back_when_their_first_folder_is_rescanned() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    let copy = tmp.path().join("laptop");
    card(&root, 4);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter::default()).unwrap();
    svc.decide(q[0].id, Decision::Favorite).unwrap();

    for e in fs::read_dir(root.join("DCIM")).unwrap() {
        let e = e.unwrap();
        fs::create_dir_all(copy.join("DCIM")).unwrap();
        fs::copy(e.path(), copy.join("DCIM").join(e.file_name())).unwrap();
    }
    let laptop = svc.add_source(Arc::new(FsSource::new(&copy).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    svc.remove_source(&laptop.id).unwrap();

    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    assert_eq!(q.len(), 4, "card photos (and their decisions) hidden after the rescan");
    assert_eq!(svc.keepers(ExportOptions::default()).unwrap().len(), 1, "the favourite can't be exported");
}

#[test]
fn export_refuses_a_destination_that_differs_from_a_source_only_in_letter_case() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Card");
    card(&root, 2);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    let dest = tmp.path().join("CARD/dcim/keepers");
    let target = CopyToDir::new(&dest).protecting(svc.known_source_ids().unwrap());
    assert!(svc.export(&target, ExportOptions::default(), &NullSink).is_err());
    assert!(!root.join("DCIM/keepers").exists());
}

/// A file renamed after the scan: export reports the photo it can't find (never drops it silently),
/// and the next rescan finds it under its new name.
#[test]
fn a_renamed_file_is_reported_by_export_and_found_by_the_next_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 2);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    fs::rename(root.join("DCIM/DSC_0000.JPG"), root.join("DCIM/RENAMED.JPG")).unwrap();

    let target = || CopyToDir::new(tmp.path().join("out")).protecting(svc.known_source_ids().unwrap());
    let r = svc.export(&target(), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r.copied, r.failed.len()), (1, 1));

    svc.scan(&Default::default(), &NullSink).unwrap();
    let r = svc.export(&target(), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r.copied, r.already_there, r.failed.len()), (1, 1, 0));
}

/// A source whose reader panics on one file (stands in for a decoder bug on a corrupt file).
struct PanicsOn(FsSource, &'static str);
impl MediaSource for PanicsOn {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn display_name(&self) -> &str {
        self.0.display_name()
    }
    fn list(&self) -> Result<Vec<MediaEntry>> {
        self.0.list()
    }
    fn open(&self, rel: &str) -> Result<Box<dyn ReadSeek>> {
        if rel.ends_with(self.1) {
            panic!("decoder bug");
        }
        self.0.open(rel)
    }
}

#[test]
fn one_crashing_file_does_not_break_every_later_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 3);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(PanicsOn(FsSource::new(&root).unwrap(), "DSC_0001.JPG"))).unwrap();
    let first = catch_unwind(AssertUnwindSafe(|| svc.scan(&Default::default(), &NullSink))).expect("the scan survives").unwrap();
    assert_eq!((first.new, first.failed.len()), (2, 1), "the crashing file is reported, the others are kept");
    let second = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(second.failed.len(), 1, "later scans still run (and retry that file)");
}

fn complete(jpeg: &[u8]) -> bool {
    jpeg.starts_with(&[0xFF, 0xD8]) && jpeg.ends_with(&[0xFF, 0xD9])
}

/// A preview cut short (power loss while the cache was being written) is noticed and rebuilt from
/// the original, never shown or kept as if it were complete.
#[test]
fn a_damaged_preview_is_rebuilt_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    let cache = tmp.path().join("cache");
    card(&root, 2);
    let svc = CullerService::in_memory(&cache).unwrap();
    let src = svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    for e in fs::read_dir(&cache).unwrap() {
        let path = e.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    }
    // Viewing with the card in: rebuilt from the original.
    for p in svc.queue(&Filter::default()).unwrap() {
        assert!(complete(&svc.preview(p.id, 2048).unwrap()), "half a preview shown (decoders fill the rest with grey)");
    }
    // A rescan also notices damaged masters and rebuilds them.
    for e in fs::read_dir(&cache).unwrap() {
        let path = e.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    }
    svc.scan(&Default::default(), &NullSink).unwrap();
    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    svc.remove_source(&src.id).unwrap();
    for id in ids {
        assert!(complete(&svc.preview(id, 2048).unwrap()), "damaged preview kept after a rescan");
    }
}

/// Someone adds their whole home folder, which contains the app's own preview cache. The scan must not
/// index the app's previews as photos (and then make previews of previews on every rescan).
#[test]
fn a_folder_containing_the_apps_own_cache_does_not_index_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    card(&home.join("Pictures"), 3);
    let cache = home.join("Library/Caches/app.darkroom.desktop/previews");
    let db = home.join("Library/Application Support/app.darkroom.desktop/darkroom.db");
    let svc = CullerService::open(&db, &cache).unwrap();
    fs::copy(root_photo(&home), db.parent().unwrap().join("stray.jpg")).unwrap(); // anything in the app's data folder
    // What the desktop app does when a folder is added.
    svc.add_source(Arc::new(FsSource::new(&home).unwrap().excluding(svc.app_dirs()))).unwrap();
    let mut found = Vec::new();
    for _ in 0..3 {
        found.push(svc.scan(&Default::default(), &NullSink).unwrap().found);
    }
    assert_eq!(found, vec![3, 3, 3], "photos found per scan");
}

fn root_photo(home: &Path) -> std::path::PathBuf {
    home.join("Pictures/DCIM/DSC_0000.JPG")
}

/// A huge file named like a photo (a misnamed disk image, a video…) is reported, never read into memory.
#[test]
fn an_oversized_file_is_reported_without_being_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 2);
    fs::File::create(root.join("DCIM/BIG.JPG")).unwrap().set_len(scan::MAX_PHOTO_BYTES + 1).unwrap(); // sparse: no disk space used
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    let started = std::time::Instant::now();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(r.new, 2);
    assert_eq!(r.failed.len(), 1);
    assert!(r.failed[0].0.ends_with("BIG.JPG") && r.failed[0].1.contains("too large"), "{:?}", r.failed);
    assert!(started.elapsed().as_secs() < 2, "the big file was read");
}

#[test]
fn export_refuses_a_destination_that_is_not_a_full_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 1);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let p = svc.queue(&Filter::default()).unwrap().remove(0);
    svc.decide(p.id, Decision::Like).unwrap();
    for dest in ["", "relative-export-folder"] {
        assert!(svc.export(&CopyToDir::new(dest), ExportOptions::default(), &NullSink).is_err(), "{dest:?}");
    }
    assert!(!Path::new("relative-export-folder").exists());
}

/// Rebuilding previews for already-indexed photos (after "Clear previews") reports files it can't
/// read, instead of silently leaving them without previews.
#[test]
fn a_preview_rebuild_that_fails_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 2);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    svc.clear_previews(&|_| false).unwrap();

    let locked = root.join("DCIM/DSC_0001.JPG");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
    assert!(r.failed[0].0.ends_with("DSC_0001.JPG"));
}

// ---- Card pulled out mid-read ----------------------------------------------------

/// A file whose reads fail part-way, like a card pulled out while it's being read.
struct CutsOut(fs::File, usize);
impl std::io::Read for CutsOut {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.1 == 0 {
            return Err(std::io::Error::other("card removed"));
        }
        let n = buf.len().min(self.1);
        let n = self.0.read(&mut buf[..n])?;
        self.1 -= n;
        Ok(n)
    }
}
impl std::io::Seek for CutsOut {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// A source whose files can be made to fail part-way through reading.
struct Pullable {
    inner: FsSource,
    root: std::path::PathBuf,
    pulled: std::sync::atomic::AtomicBool,
}
impl MediaSource for Pullable {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn list(&self) -> Result<Vec<MediaEntry>> {
        self.inner.list()
    }
    fn open(&self, rel: &str) -> Result<Box<dyn ReadSeek>> {
        if self.pulled.load(std::sync::atomic::Ordering::Relaxed) && rel.ends_with("DSC_0001.JPG") {
            return Ok(Box::new(CutsOut(fs::File::open(self.root.join(rel))?, 1000)));
        }
        self.inner.open(rel)
    }
}

#[test]
fn a_card_pulled_out_mid_read_fails_that_file_and_keeps_everything_else() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 3);
    let src = Arc::new(Pullable { inner: FsSource::new(&root).unwrap(), root: root.clone(), pulled: true.into() });
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(src.clone()).unwrap();

    // Scan: the cut-off file is reported, never indexed half-read; the others are kept.
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!((r.new, r.failed.len()), (2, 1), "{:?}", r.failed);
    // Card back in: the next scan picks it up.
    src.pulled.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(svc.scan(&Default::default(), &NullSink).unwrap().new, 1);

    // Export: the cut-off copy is reported and leaves nothing behind in the destination.
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    src.pulled.store(true, std::sync::atomic::Ordering::Relaxed);
    let out = tmp.path().join("out");
    let r = svc.export(&CopyToDir::new(&out), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r.copied, r.failed.len()), (2, 1), "{:?}", r.failed);
    let mut names: Vec<_> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["DSC_0000.JPG", "DSC_0002.JPG"], "no partial copy of the cut-off file");
    for n in &names {
        assert_eq!(fs::read(out.join(n)).unwrap(), fs::read(root.join("DCIM").join(n)).unwrap());
    }
}

/// Symlinks on a card (to a file or a folder elsewhere) are never followed: only real files
/// inside the folder are photos.
#[test]
fn symlinks_inside_a_source_are_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    let elsewhere = tmp.path().join("elsewhere");
    card(&root, 1);
    card(&elsewhere, 3);
    std::os::unix::fs::symlink(elsewhere.join("DCIM/DSC_0002.JPG"), root.join("DCIM/LINK.JPG")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("DCIM"), root.join("LINKED_DIR")).unwrap();
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!((r.found, r.new), (1, 1));
}

/// Accents, spaces and non-Latin names go through scan, preview and export unchanged.
#[test]
fn unusual_file_names_go_through_the_whole_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Vacances été 2026");
    let names = ["Été à Paris 01.JPG", "写真 02.JPG", "a b  c.jpg"];
    for (i, n) in names.iter().enumerate() {
        write_jpeg(&root.join("DCIM").join(n), 320, 240, i as u8 * 40);
    }
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    assert_eq!(svc.scan(&Default::default(), &NullSink).unwrap().new, 3);
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.preview(p.id, 800).unwrap();
        svc.decide(p.id, Decision::Like).unwrap();
    }
    let out = tmp.path().join("Keepers à moi");
    assert_eq!(svc.export(&CopyToDir::new(&out), ExportOptions::default(), &NullSink).unwrap().copied, 3);
    for n in names {
        assert_eq!(fs::read(out.join(n)).unwrap(), fs::read(root.join("DCIM").join(n)).unwrap(), "{n}");
    }
}

/// Decisions live in the database file: they survive closing and reopening the app.
#[test]
fn decisions_survive_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 3);
    let (db, cache) = (tmp.path().join("data/darkroom.db"), tmp.path().join("cache"));
    {
        let svc = CullerService::open(&db, &cache).unwrap();
        svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
        svc.scan(&Default::default(), &NullSink).unwrap();
        let q = svc.queue(&Filter::default()).unwrap();
        svc.decide(q[0].id, Decision::Favorite).unwrap();
        svc.decide(q[1].id, Decision::Hate).unwrap();
    }
    let svc = CullerService::open(&db, &cache).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(r.new, 0, "nothing re-indexed");
    let all = svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    let mut decisions: Vec<_> = all.iter().map(|p| p.decision).collect();
    decisions.sort_by_key(|d| format!("{d:?}"));
    assert_eq!(decisions, [None, Some(Decision::Favorite), Some(Decision::Hate)]);
}

/// A folder is regenerated (some files gone) and previews were cleared. The rows of the vanished files have
/// nothing to show and must not come back as broken photos; photos that still exist stay, with their decisions.
#[test]
fn photos_whose_files_vanished_and_have_no_preview_are_forgotten_by_the_next_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("folder");
    card(&root, 6);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let all = || svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    assert_eq!(all().len(), 6);
    svc.decide(all()[0].id, Decision::Favorite).unwrap();

    // Two files vanish while their previews are still cached: they stay viewable, so they stay.
    fs::remove_file(root.join("DCIM/DSC_0004.JPG")).unwrap();
    fs::remove_file(root.join("DCIM/DSC_0005.JPG")).unwrap();
    // A different photo now sits at DSC_0003's path (a regenerated set reuses file names).
    write_jpeg(&root.join("DCIM/DSC_0003.JPG"), 400, 300, 200);
    svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(all().len(), 7, "the old ones are still viewable from the cache");

    // Previews cleared: now they have nothing to show, and the next scan forgets them.
    svc.clear_previews(&|_| false).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let left = all();
    assert_eq!(left.len(), 4, "0-2 and the new DSC_0003; the old DSC_0003 goes too, though its path is still listed");
    assert!(left.iter().all(|p| svc.preview(p.id, 256).is_ok()), "every photo left can be shown");
    assert!(left.iter().any(|p| p.decision == Some(Decision::Favorite)), "decisions of photos that remain are kept");
}

/// A scan asked for while another runs joins it: the call returns at once (`queued`) and the running
/// scan does it before returning, so adding a folder mid-scan never needs a second button press.
struct AddDuringScan {
    svc: OnceLock<Arc<CullerService>>,
    second: OnceLock<String>,
    queued: Mutex<Option<bool>>,
}
impl EventSink for AddDuringScan {
    fn emit(&self, event: CoreEvent) {
        if let CoreEvent::ScanSaved { .. } = event {
            let mut queued = self.queued.lock().unwrap();
            if queued.is_none() {
                let req = ScanRequest { source_ids: Some(vec![self.second.get().unwrap().clone()]) };
                *queued = Some(self.svc.get().unwrap().scan(&req, &NullSink).unwrap().queued);
            }
        }
    }
}

#[test]
fn a_scan_asked_for_during_another_runs_right_after_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    card(&a, 40);
    for i in 0..5u8 {
        write_jpeg(&b.join(format!("DCIM/B_{i:04}.JPG")), 300, 200, 100 + i * 7);
    }
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    let first = svc.add_source(Arc::new(FsSource::new(&a).unwrap())).unwrap();
    let second = svc.add_source(Arc::new(FsSource::new(&b).unwrap())).unwrap();
    let sink = AddDuringScan { svc: OnceLock::new(), second: OnceLock::new(), queued: Mutex::new(None) };
    sink.svc.set(svc.clone()).ok();
    sink.second.set(second.id.clone()).unwrap();
    let report = svc.scan(&ScanRequest { source_ids: Some(vec![first.id]) }, &sink).unwrap();
    assert_eq!(*sink.queued.lock().unwrap(), Some(true), "the second request was handed to the running scan");
    assert!(!report.queued);
    assert_eq!(report.new, 45, "and the running scan did it before returning");
    assert_eq!(svc.queue(&Filter::default()).unwrap().len(), 45);
}

#[test]
fn forgetting_a_folder_deletes_its_photos_and_previews_but_not_its_files_or_other_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    card(&a, 4);
    for i in 0..3u8 {
        write_jpeg(&b.join(format!("DCIM/B_{i:04}.JPG")), 300, 200, 100 + i * 7);
    }
    let cache = tmp.path().join("cache");
    let svc = CullerService::in_memory(&cache).unwrap();
    let sa = svc.add_source(Arc::new(FsSource::new(&a).unwrap())).unwrap();
    let sb = svc.add_source(Arc::new(FsSource::new(&b).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let all = svc.queue(&Filter::default()).unwrap();
    for p in &all {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    let dest = tmp.path().join("out");
    svc.export(&CopyToDir::new(&dest).protecting(svc.known_source_ids().unwrap()), ExportOptions::default(), &NullSink).unwrap();
    let previews_before = fs::read_dir(&cache).unwrap().count();
    let files_before: Vec<_> = walk(&a);

    let rep = svc.forget_source(&sa.id).unwrap();
    assert_eq!(rep.photos, 4);
    assert!(rep.preview_bytes > 0);
    assert_eq!(fs::read_dir(&cache).unwrap().count(), previews_before - 8, "both sizes of its 4 photos");
    assert_eq!(walk(&a), files_before, "the folder itself is untouched");
    assert!(svc.undo().unwrap().is_empty(), "undo can't bring back a forgotten photo");
    let lib = svc.library().unwrap();
    assert_eq!(lib.len(), 1, "a forgotten folder leaves the library: {lib:?}");
    assert_eq!((lib[0].id.as_str(), lib[0].photo_count), (sb.id.as_str(), 3));
    assert!(svc.known_source_ids().unwrap().contains(&sa.id), "exports still refuse its folder");

    // Adding it back starts from scratch: new photos, no decisions, no export history.
    svc.add_source(Arc::new(FsSource::new(&a).unwrap())).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(r.new, 4);
    let back = svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    assert_eq!(back.iter().filter(|p| p.source_id == sa.id && p.decision.is_none()).count(), 4);
    assert_eq!(back.iter().filter(|p| p.source_id == sb.id && p.decision == Some(Decision::Like)).count(), 3, "the other folder keeps its decisions");
}

#[test]
fn the_session_survives_a_restart_and_removed_folders_stay_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    card(&a, 2);
    card(&b, 2);
    let db = tmp.path().join("data/db");
    let ids = {
        let svc = CullerService::open(&db, &tmp.path().join("cache")).unwrap();
        let sa = svc.add_source(Arc::new(FsSource::new(&a).unwrap())).unwrap();
        let sb = svc.add_source(Arc::new(FsSource::new(&b).unwrap())).unwrap();
        svc.remove_source(&sb.id).unwrap();
        (sa.id, sb.id)
    };
    let svc = CullerService::open(&db, &tmp.path().join("cache")).unwrap();
    assert_eq!(svc.saved_session_sources().unwrap(), vec![ids.0.clone()]);
    let lib = svc.library().unwrap();
    assert!(lib.iter().any(|l| l.id == ids.0 && l.in_session));
    assert!(!lib.iter().any(|l| l.id == ids.1), "removed and without photos: nothing to show");
}

fn walk(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out: Vec<_> = walkdir::WalkDir::new(dir).into_iter().map(|e| e.unwrap()).filter(|e| e.file_type().is_file()).map(|e| (e.path().to_path_buf(), fs::read(e.path()).unwrap())).collect();
    out.sort();
    out
}
