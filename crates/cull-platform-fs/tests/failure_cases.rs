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
    svc.remove_source(&src.id); // card unplugged
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
    svc.remove_source(&laptop.id);

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

/// Renaming files keeps the folder's count and size, so a plain rescan skips it. Export must then
/// report the photos it can't find, never drop them silently; ↻ (forced) fixes it.
#[test]
fn renamed_files_in_a_skipped_folder_are_reported_by_export_and_found_by_a_forced_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 2);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    let src = svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    fs::rename(root.join("DCIM/DSC_0000.JPG"), root.join("DCIM/RENAMED.JPG")).unwrap();
    assert_eq!(svc.scan(&Default::default(), &NullSink).unwrap().unchanged_sources, 1);

    let target = || CopyToDir::new(tmp.path().join("out")).protecting(svc.known_source_ids().unwrap());
    let r = svc.export(&target(), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r.copied, r.failed.len()), (1, 1));

    svc.scan(&ScanRequest { source_ids: Some(vec![src.id]), force: true }, &NullSink).unwrap();
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
    // A forced rescan also notices damaged masters and rebuilds them.
    for e in fs::read_dir(&cache).unwrap() {
        let path = e.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    }
    svc.scan(&ScanRequest { source_ids: Some(vec![src.id.clone()]), force: true }, &NullSink).unwrap();
    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    svc.remove_source(&src.id);
    for id in ids {
        assert!(complete(&svc.preview(id, 2048).unwrap()), "damaged preview kept after a forced rescan");
    }
}

/// Someone adds their whole home folder, which contains the app's own preview cache. The scan must not
/// index the app's previews as photos (and then make previews of previews on every rescan).
#[test]
fn a_folder_containing_the_apps_own_cache_does_not_index_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    card(&home.join("Pictures"), 3);
    let cache = home.join("Library/Caches/app.cameratinder.desktop/previews");
    let db = home.join("Library/Application Support/app.cameratinder.desktop/camera-tinder.db");
    let svc = CullerService::open(&db, &cache).unwrap();
    fs::copy(root_photo(&home), db.parent().unwrap().join("stray.jpg")).unwrap(); // anything in the app's data folder
    // What the desktop app does when a folder is added.
    let src = svc.add_source(Arc::new(FsSource::new(&home).unwrap().excluding(svc.app_dirs()))).unwrap();
    let force = || ScanRequest { source_ids: Some(vec![src.id.clone()]), force: true };
    let mut found = Vec::new();
    for _ in 0..3 {
        found.push(svc.scan(&force(), &NullSink).unwrap().found);
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
