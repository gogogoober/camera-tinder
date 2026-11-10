//! Guarantees that the app never changes, moves or deletes original photos.
//!
//! The only allowed contact with a source is *reading* it (scan, previews,
//! export copy). These tests run the whole flow against sources and check,
//! byte for byte, that nothing on the source side changed.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cull_core::*;
use cull_platform_fs::{CopyToDir, FsSource};
use image::{ImageBuffer, Rgb};

fn write_jpeg(path: &Path, w: u32, h: u32, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([shade, (x % 256) as u8, (y % 256) as u8]));
    img.save(path).unwrap();
}

/// Every entry under `root` (files, dirs, symlinks) → (full content hash, len, mtime, permissions).
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, u64, std::time::SystemTime, u32)> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(root).follow_links(false) {
        let e = e.unwrap();
        let m = fs::symlink_metadata(e.path()).unwrap();
        let hash = if m.is_file() { xxhash_rust::xxh3::xxh3_64(&fs::read(e.path()).unwrap()) } else { 0 };
        out.insert(e.path().strip_prefix(root).unwrap().to_path_buf(), (hash, m.len(), m.modified().unwrap(), m.permissions().mode()));
    }
    out
}

/// A camera card: a few real JPEGs, a burst, a RAW, junk and hidden files.
fn make_card(root: &Path) {
    write_jpeg(&root.join("DCIM/100NIKON/DSC_0001.JPG"), 1600, 1200, 10);
    write_jpeg(&root.join("DCIM/100NIKON/DSC_0002.JPG"), 1600, 1200, 60);
    write_jpeg(&root.join("DCIM/100NIKON/DSC_0003.JPG"), 1200, 1600, 120);
    write_jpeg(&root.join("DCIM/101FUJI/DSCF0001.JPG"), 1600, 1200, 200);
    fs::write(root.join("DCIM/100NIKON/DSC_0003.NEF"), b"pretend raw").unwrap();
    fs::write(root.join("DCIM/.hidden.jpg"), b"junk").unwrap();
    fs::write(root.join("notes.txt"), b"hi").unwrap();
}

fn set_tree_mode(root: &Path, file: u32, dir: u32) {
    // Deepest first so we can still descend while locking.
    let mut entries: Vec<_> = walkdir::WalkDir::new(root).into_iter().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.depth()));
    for e in entries {
        let mode = if e.file_type().is_dir() { dir } else { file };
        fs::set_permissions(e.path(), fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// Runs every operation that touches photos: scan, rescan, previews,
/// all four decisions, bulk decisions, undo, tags, suggestions, export.
fn exercise_everything(svc: &CullerService, dest: &Path) -> ExportReport {
    svc.scan(&Default::default(), &NullSink).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap(); // rescan (cached path)
    let q = svc.queue(&Filter::default()).unwrap();
    assert_eq!(q.len(), 4);
    for p in &q {
        svc.preview(p.id, 512).unwrap();
        svc.preview(p.id, 2048).unwrap();
    }
    svc.plan(&Filter::default()).unwrap();
    svc.decide(q[0].id, Decision::Hate).unwrap();
    svc.decide(q[1].id, Decision::Dislike).unwrap();
    svc.decide(q[2].id, Decision::Favorite).unwrap();
    svc.decide(q[3].id, Decision::Like).unwrap();
    svc.decide_many(&[q[0].id, q[1].id], Decision::Hate).unwrap();
    svc.undo().unwrap();
    svc.decide_many(&[q[0].id, q[1].id], Decision::Hate).unwrap();
    svc.add_tag("Trip", "2020-01-01", "2030-01-01", TagSource::Manual).unwrap();
    svc.suggest_tags().unwrap();
    svc.export(&CopyToDir::new(dest).protecting(svc.known_source_ids().unwrap()), ExportOptions::default(), &NullSink).unwrap()
}

#[test]
fn full_flow_leaves_source_byte_for_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let before = snapshot(&card);

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    let report = exercise_everything(&svc, &tmp.path().join("keepers"));
    assert_eq!(report.copied, 2, "{:?}", report.failed);
    svc.remove_source(&card.canonicalize().unwrap().to_string_lossy());

    assert_eq!(snapshot(&card), before, "source tree changed: contents, size, mtime, permissions or file list");
}

#[test]
fn works_on_a_read_only_source() {
    // A write-protected card (lock switch) or read-only folder: every feature still works,
    // which proves none of them need write access to the source.
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    set_tree_mode(&card, 0o444, 0o555);
    let before = snapshot(&card);

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    let report = exercise_everything(&svc, &tmp.path().join("keepers"));
    assert_eq!(report.copied, 2, "{:?}", report.failed);
    assert_eq!(snapshot(&card), before);

    set_tree_mode(&card, 0o644, 0o755); // let tempdir clean up
}

#[test]
fn hated_and_rejected_photos_are_never_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    svc.decide_many(&ids, Decision::Hate).unwrap();
    // Exporting with everything hated copies nothing and deletes nothing.
    let r = svc.export(&CopyToDir::new(tmp.path().join("out")), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 0);
    for f in ["DSC_0001.JPG", "DSC_0002.JPG", "DSC_0003.JPG", "DSC_0003.NEF"] {
        assert!(card.join("DCIM/100NIKON").join(f).exists(), "{f} was removed");
    }
    // Hates are recorded, not acted on.
    assert_eq!(svc.stats().unwrap().hate, 4);
}

#[test]
fn export_refuses_destinations_inside_a_source() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let before = snapshot(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    svc.decide_many(&ids, Decision::Like).unwrap();

    // A symlink pointing into the card must not sneak past the check either.
    let sneaky = tmp.path().join("looks-safe");
    std::os::unix::fs::symlink(card.join("DCIM"), &sneaky).unwrap();

    let roots = svc.known_source_ids().unwrap();
    for dest in [card.clone(), card.join("DCIM/100NIKON"), card.join("new/keepers"), sneaky.join("keepers"), tmp.path().join("x/../card/k")] {
        let r = svc.export(&CopyToDir::new(&dest).protecting(&roots), ExportOptions::default(), &NullSink);
        assert!(r.is_err(), "export into {} should be refused", dest.display());
    }
    assert_eq!(snapshot(&card), before);

    // Sources stay protected after they're removed from the session.
    svc.remove_source(&roots[0]);
    let roots = svc.known_source_ids().unwrap();
    assert!(svc.export(&CopyToDir::new(&card).protecting(&roots), ExportOptions::default(), &NullSink).is_err());
}

#[test]
fn export_never_overwrites_existing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter::default()).unwrap();
    let ids: Vec<_> = q.iter().map(|p| p.id).collect();
    svc.decide_many(&ids, Decision::Like).unwrap();

    // The destination already holds files with the same names: one with different
    // bytes but the *same size* (the old check would have skipped it as "already exported").
    let dest = tmp.path().join("keepers");
    fs::create_dir_all(&dest).unwrap();
    let p1 = q.iter().find(|p| p.file_name == "DSC_0001.JPG").unwrap();
    let decoy = vec![0xAB; p1.size as usize];
    fs::write(dest.join("DSC_0001.JPG"), &decoy).unwrap();
    fs::write(dest.join("DSC_0002.JPG"), b"someone else's photo").unwrap();

    let r = svc.export(&CopyToDir::new(&dest), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r.copied, r.already_there), (4, 0), "{:?}", r.failed);
    assert_eq!(fs::read(dest.join("DSC_0001.JPG")).unwrap(), decoy, "existing file was overwritten");
    assert_eq!(fs::read(dest.join("DSC_0002.JPG")).unwrap(), b"someone else's photo");
    assert_eq!(fs::read(dest.join("DSC_0001-1.JPG")).unwrap(), fs::read(card.join("DCIM/100NIKON/DSC_0001.JPG")).unwrap());
    assert!(dest.join("DSC_0002-1.JPG").exists());

    // Exported copies are exact, and no temp files are left behind.
    let again = svc.export(&CopyToDir::new(&dest), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((again.copied, again.already_there), (0, 4));
    assert!(fs::read_dir(&dest).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".partial")));
}

#[test]
fn export_skips_files_changed_since_scan() {
    // If a source file changes after it was culled, we must not export it as the keeper
    // the user chose (and must not "fix" the source either).
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let ids: Vec<_> = svc.queue(&Filter::default()).unwrap().iter().map(|p| p.id).collect();
    svc.decide_many(&ids, Decision::Like).unwrap();

    // Edited elsewhere after the scan, same size (e.g. bit rot or an in-place EXIF tool):
    // only the content check can catch this one.
    let edited = card.join("DCIM/100NIKON/DSC_0002.JPG");
    let mut edited_bytes = fs::read(&edited).unwrap();
    let n = edited_bytes.len();
    for b in &mut edited_bytes[n - 200..n - 2] {
        *b ^= 0x55;
    }
    fs::write(&edited, &edited_bytes).unwrap();
    // Also a resized one (size check).
    write_jpeg(&card.join("DCIM/100NIKON/DSC_0003.JPG"), 1200, 1600, 121);

    let dest = tmp.path().join("keepers");
    let r = svc.export(&CopyToDir::new(&dest), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 2);
    assert_eq!(r.failed.len(), 2, "{:?}", r.failed);
    assert!(!dest.join("DSC_0002.JPG").exists());
    assert!(!dest.join("DSC_0003.JPG").exists());
    assert_eq!(fs::read(&edited).unwrap(), edited_bytes);
}

/// Static guard: the only filesystem-writing calls in the codebase are in the places
/// audited for source safety. A new `remove_file`, `rename`, `fs::write`… anywhere
/// else fails this test until someone reviews it and adds it here.
#[test]
fn only_audited_code_writes_to_disk() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let allowed: &[(&str, &[&str])] = &[
        // Export: writes only inside the (checked) destination; removes only its own temp file.
        // The exFAT path claims the final name with a new empty file and removes only that empty claim.
        ("crates/cull-platform-fs/src/lib.rs", &["create_dir_all(&dest)", "OpenOptions::new().write(true).create_new(true).open(&tmp)", "OpenOptions::new().write(true).create_new(true).open(&target)", "fs::hard_link(&tmp, &target)", "fs::rename(&tmp, &target)", "fs::remove_file(&tmp)", "fs::remove_file(&target); // our own empty claim"]),
        // Preview cache: app cache dir only. Temp files are created new (never an existing file).
        // `clear` removes only cache-named regular files directly in the cache dir.
        ("crates/cull-core/src/preview.rs", &["fs::create_dir_all(&dir)", "OpenOptions::new().write(true).create_new(true).open(&tmp)", "fs::rename(&tmp, path)", "fs::remove_file(&tmp); // our own temp file only", "fs::remove_file(&path)"]),
        // Database folder and file: app data dir only.
        ("crates/cull-core/src/api.rs", &["std::fs::create_dir_all(parent)"]),
        ("crates/cull-core/src/store.rs", &["Connection::open(path)"]),
    ];
    let needles = [
        "remove_file", "remove_dir", "fs::write", "fs::copy", "File::create", "File::options", "rename(", "renamex", "hard_link", "symlink(", "create_dir",
        "set_permissions", "set_len", ".write(true)", ".append(true)", ".truncate(", "Command::new", "set_modified", "set_times", "unlink", "libc::",
        "Connection::open(", "OpenFlags",
        // Apple APIs that write to a path or URL (previews/analysis must stay in memory).
        "CreateWithURL", "CFURL", "NSURL", "writeTo", "removeItem", "moveItem", "copyItem", "createFile", "createDirectory",
    ];

    let mut problems = Vec::new();
    for dir in ["crates/cull-core/src", "crates/cull-platform-fs/src", "crates/cull-platform-apple/src", "src-tauri/src"] {
        for e in walkdir::WalkDir::new(repo.join(dir)) {
            let e = e.unwrap();
            if e.path().extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let rel = e.path().strip_prefix(&repo).unwrap().to_string_lossy().replace('\\', "/");
            let text = fs::read_to_string(e.path()).unwrap();
            // Unit tests inside a file may write to their own temp dirs.
            let code = text.split("#[cfg(test)]").next().unwrap();
            let ok = allowed.iter().find(|(f, _)| *f == rel).map(|(_, a)| *a).unwrap_or(&[]);
            for (n, line) in code.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                if needles.iter().any(|w| t.contains(w)) && !ok.iter().any(|a| t.contains(a)) {
                    problems.push(format!("{rel}:{}: {t}", n + 1));
                }
            }
        }
    }
    assert!(problems.is_empty(), "unaudited filesystem writes:\n{}", problems.join("\n"));
}

#[test]
fn sources_are_opened_read_only() {
    // The handle is `Read + Seek` only (no Write in the type), opened with read(true) only.
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let src = FsSource::new(&card).unwrap();
    let mut r = src.open("DCIM/100NIKON/DSC_0001.JPG").unwrap();
    let mut buf = [0u8; 4];
    std::io::Read::read_exact(&mut r, &mut buf).unwrap();
    assert_eq!(&buf[..2], &[0xFF, 0xD8]);
    // Path tricks can't escape the source root.
    assert!(src.open("../card/DCIM/100NIKON/DSC_0001.JPG").is_err());
    assert!(src.open("/etc/hosts").is_err());
    assert!(src.open("DCIM/../notes.txt").is_err());
}

// ---- When the card is touched -------------------------------------------------
//
// The goal: read each original once at scan time, then leave the card alone for
// the whole culling session. Export is the only other moment it is read.

/// Wraps a source and counts every file it opens.
struct Counting {
    inner: FsSource,
    opens: std::sync::Mutex<Vec<String>>,
}
impl Counting {
    fn new(root: &Path) -> Arc<Self> {
        Arc::new(Self { inner: FsSource::new(root).unwrap(), opens: Default::default() })
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.opens.lock().unwrap())
    }
}
impl MediaSource for Counting {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn list(&self) -> Result<Vec<MediaEntry>> {
        self.inner.list()
    }
    fn open(&self, rel_path: &str) -> Result<Box<dyn ReadSeek>> {
        self.opens.lock().unwrap().push(rel_path.to_string());
        self.inner.open(rel_path)
    }
}

#[test]
fn card_is_read_once_at_scan_then_left_alone_until_export() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let src = Counting::new(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(src.clone()).unwrap();

    // Scan: every JPEG opened exactly once (fingerprint, EXIF, previews all from one read).
    let report = svc.scan(&Default::default(), &NullSink).unwrap();
    let mut opened = src.take();
    opened.sort();
    assert_eq!(opened, ["DCIM/100NIKON/DSC_0001.JPG", "DCIM/100NIKON/DSC_0002.JPG", "DCIM/100NIKON/DSC_0003.JPG", "DCIM/101FUJI/DSCF0001.JPG"]);
    assert_eq!((report.skipped_unsupported, report.other_files), (1, 1), "NEF + notes.txt are reported, not ignored silently");

    // Rescan of an unchanged card: no file contents read at all.
    svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(src.take(), Vec::<String>::new(), "rescan should only list the card");

    // A whole culling session: every preview size the UI uses (and silly ones), all decisions,
    // bulk, undo, plan, tags, suggestions. The card must not be opened once.
    let q = svc.queue(&Filter::default()).unwrap();
    for p in &q {
        for edge in [64, 256, 512, 1024, 1600, 2048, 4096, 9999] {
            let jpeg = svc.preview(p.id, edge).unwrap();
            let img = image::load_from_memory(&jpeg).unwrap();
            assert!(img.width().max(img.height()) <= edge.max(64).min(2048));
        }
    }
    svc.plan(&Filter::default()).unwrap();
    svc.decide(q[0].id, Decision::Hate).unwrap();
    svc.decide(q[1].id, Decision::Like).unwrap();
    svc.decide_many(&[q[2].id, q[3].id], Decision::Favorite).unwrap();
    svc.undo().unwrap();
    svc.decide_many(&[q[2].id, q[3].id], Decision::Favorite).unwrap();
    svc.add_tag("Trip", "2020-01-01", "2030-01-01", TagSource::Manual).unwrap();
    svc.suggest_tags().unwrap();
    svc.days().unwrap();
    svc.stats().unwrap();
    assert_eq!(src.take(), Vec::<String>::new(), "culling must not touch the card");

    // Export: each keeper read once to copy it.
    let r = svc.export(&CopyToDir::new(tmp.path().join("keepers")), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 3, "{:?}", r.failed);
    assert_eq!(src.take().len(), 3);
}

#[test]
fn culling_works_with_the_card_unplugged_and_export_reports_what_it_could_not_reach() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    make_card(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter::default()).unwrap();

    // Unplug the card mid-session (the source stays in the session, its files vanish).
    fs::rename(&card, tmp.path().join("card-away")).unwrap();

    // Previews still come from the cache...
    for p in &q {
        svc.preview(p.id, 256).unwrap();
        svc.preview(p.id, 1024).unwrap();
        svc.preview(p.id, 2048).unwrap();
    }
    // ...and decisions still record.
    svc.decide_many(&q.iter().map(|p| p.id).collect::<Vec<_>>(), Decision::Like).unwrap();

    // Export can't reach the card: every keeper is listed as failed, none silently dropped.
    fn dest_is_empty(d: &Path) -> bool {
        fs::read_dir(d).map(|mut r| r.next().is_none()).unwrap_or(true)
    }
    let r = svc.export(&CopyToDir::new(tmp.path().join("keepers")), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 0);
    assert_eq!(r.failed.len(), 4, "{:?}", r.failed);
    assert!(dest_is_empty(&tmp.path().join("keepers")), "no partial files left behind");
}

#[test]
fn older_index_without_previews_is_backfilled_by_one_read_on_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    let cache = tmp.path().join("cache");
    make_card(&card);
    let src = Counting::new(&card);
    let svc = CullerService::in_memory(&cache).unwrap();
    svc.add_source(src.clone()).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    src.take();

    // Simulate a library indexed by an older version: no cached previews.
    for e in fs::read_dir(&cache).unwrap() {
        fs::remove_file(e.unwrap().path()).unwrap();
    }
    svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(src.take().len(), 4, "one read per photo to rebuild previews");
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.preview(p.id, 1024).unwrap();
    }
    assert_eq!(src.take(), Vec::<String>::new());
}

// ---- Clearing the preview cache ------------------------------------------------
//
// "Clear previews" must only ever delete the app's own preview copies, and only while
// no card or external drive with photos is plugged in.

fn cache_files(cache: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(cache).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn clearing_previews_waits_until_external_sources_are_unplugged() {
    // The platform decides what "external" means; here the test plays the platform:
    // the card is external (blocks while plugged in), the Pictures folder is internal.
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    let folder = tmp.path().join("Pictures/Trip");
    let cache = tmp.path().join("cache");
    make_card(&card);
    write_jpeg(&folder.join("IMG_1.JPG"), 800, 600, 30);
    let svc = CullerService::in_memory(&cache).unwrap();
    let card_src = Counting::new(&card);
    svc.add_source(card_src.clone()).unwrap();
    svc.add_source(Arc::new(FsSource::new(&folder).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    card_src.take();
    let q = svc.queue(&Filter::default()).unwrap();
    svc.decide(q[0].id, Decision::Favorite).unwrap();
    svc.add_tag("Trip", "2020-01-01", "2030-01-01", TagSource::Manual).unwrap();

    let card_id = card.canonicalize().unwrap().to_string_lossy().into_owned();
    let external = |id: &str| id == card_id && cull_platform_fs::is_reachable(id);

    let st = svc.preview_cache_status(&external).unwrap();
    assert_eq!(st.files, 10, "master + thumb for 5 photos");
    assert_eq!(st.connected_sources, vec!["card".to_string()], "internal folders never block");
    assert!(svc.clear_previews(&external).is_err());
    assert_eq!(svc.preview_cache_status(&external).unwrap().files, 10, "refusal deletes nothing");

    // Removing the card from the session isn't unplugging it.
    svc.remove_source(&card_id);
    assert!(svc.clear_previews(&external).is_err());

    // Unplug: allowed. Only previews go; photos, decisions and tags stay; card never read.
    let away = tmp.path().join("card-away");
    fs::rename(&card, &away).unwrap();
    let (card_before, folder_before) = (snapshot(&away), snapshot(&folder));
    let freed = svc.clear_previews(&external).unwrap();
    assert_eq!(freed, st.bytes);
    assert_eq!(cache_files(&cache), Vec::<String>::new());
    assert_eq!(snapshot(&away), card_before);
    assert_eq!(snapshot(&folder), folder_before);
    assert_eq!(card_src.take(), Vec::<String>::new());
    assert_eq!(svc.tags().unwrap().len(), 1);

    // Plug back in and rescan: previews rebuilt with one read per photo.
    fs::rename(&away, &card).unwrap();
    svc.add_source(card_src.clone()).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(card_src.take().len(), 4);
    assert_eq!(svc.stats().unwrap().favorite, 1, "decisions survive clearing");
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.preview(p.id, 1024).unwrap();
    }
    assert_eq!(card_src.take(), Vec::<String>::new());
}

#[test]
fn clearing_previews_deletes_nothing_but_preview_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    let cache = tmp.path().join("cache");
    make_card(&card);
    let svc = CullerService::in_memory(&cache).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let ours = cache_files(&cache);
    assert_eq!(ours.len(), 8);

    // Decoys: things that are not preview copies, some made to look like them.
    let photo = card.join("DCIM/100NIKON/DSC_0001.JPG");
    let photo_bytes = fs::read(&photo).unwrap();
    let fake = |edge: u32| format!("{}_{edge}.jpg", "f".repeat(32));
    fs::copy(&photo, cache.join("DSC_0001.JPG")).unwrap(); // a real photo dropped in the folder
    std::os::unix::fs::symlink(&photo, cache.join(fake(2048))).unwrap(); // preview-named symlink → original
    fs::hard_link(&photo, cache.join(fake(999))).unwrap(); // preview-named hard link → original
    fs::create_dir(cache.join(fake(256))).unwrap(); // preview-named folder…
    fs::write(cache.join(fake(256)).join("inside.jpg"), b"x").unwrap(); // …with a file in it
    fs::write(cache.join(".notes.tmp"), b"x").unwrap();
    fs::write(cache.join("ABCDEF0123456789ABCDEF0123456789_256.jpg"), b"x").unwrap();
    fs::write(cache.join(".DS_Store"), b"x").unwrap();

    let nothing_connected = |_: &str| false;
    svc.clear_previews(&nothing_connected).unwrap();

    let left = cache_files(&cache);
    for name in &ours {
        assert!(!left.contains(name), "{name} should be cleared");
    }
    let mut expected = vec![
        "DSC_0001.JPG".to_string(),
        fake(2048),
        fake(256),
        ".notes.tmp".into(),
        "ABCDEF0123456789ABCDEF0123456789_256.jpg".into(),
        ".DS_Store".into(),
    ];
    expected.sort();
    // The only decoy that goes is the hard link's *name*; removing a name never removes data
    // reachable through another link, so the original is intact (checked below).
    assert_eq!(left, expected);
    assert_eq!(fs::read(&photo).unwrap(), photo_bytes, "original untouched");
    assert!(fs::symlink_metadata(cache.join(fake(2048))).unwrap().file_type().is_symlink());
    assert!(cache.join(fake(256)).join("inside.jpg").exists());
    assert_eq!(fs::read(cache.join("DSC_0001.JPG")).unwrap(), photo_bytes);

    // A cache folder that is itself a symlink to somewhere else is refused outright.
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let real = elsewhere.join(fake(512));
    fs::write(&real, b"keep").unwrap();
    let linked = tmp.path().join("linked-cache");
    std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
    let svc2 = CullerService::in_memory(&linked).unwrap();
    assert!(svc2.clear_previews(&nothing_connected).is_err());
    assert_eq!(fs::read(&real).unwrap(), b"keep");
}

#[test]
fn the_macs_own_drive_never_counts_as_external() {
    use cull_platform_fs::is_external_and_connected as ext;
    let tmp = tempfile::tempdir().unwrap();
    let home = std::env::var("HOME").unwrap();
    let pictures = format!("{home}/Pictures");
    for p in ["/", "/Users", home.as_str(), pictures.as_str(), tmp.path().to_str().unwrap()] {
        assert!(!ext(p), "{p} is on the Mac's own drive");
    }
    // Not mounted / not there: not connected.
    assert!(!ext("/Volumes/NO_SUCH_CARD_7f3a"));
    assert!(!ext(&format!("{home}/no-such-folder-7f3a")));
}

/// Mounts a real disk image (what macOS does with an SD card) and checks it counts as
/// external while mounted and disconnected after ejecting. Needs `hdiutil` (macOS):
/// `cargo test -p cull-platform-fs --test source_safety -- --ignored`
#[test]
#[ignore]
fn a_mounted_volume_counts_as_external_until_ejected() {
    use std::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("card.dmg");
    let name = format!("CTTEST{}", std::process::id());
    let ok = |c: &mut Command| c.status().unwrap().success();
    assert!(ok(Command::new("hdiutil").args(["create", "-quiet", "-size", "8m", "-fs", "ExFAT", "-volname", &name]).arg(&img)));
    assert!(ok(Command::new("hdiutil").args(["attach", "-quiet", "-nobrowse"]).arg(&img)));
    let mount = format!("/Volumes/{name}");
    let dcim = format!("{mount}/DCIM");
    fs::create_dir_all(&dcim).unwrap();
    let id = fs::canonicalize(&dcim).unwrap().to_string_lossy().into_owned();
    let mounted = cull_platform_fs::is_external_and_connected(&id);
    assert!(ok(Command::new("hdiutil").args(["detach", "-quiet", &mount])));
    assert!(mounted, "{id} should count as external while mounted");
    assert!(!cull_platform_fs::is_external_and_connected(&id), "ejected volume is not connected");
}

/// A mounted disk image, ejected when dropped (even if the test fails).
struct Volume {
    mount: String,
    image: PathBuf,
}
impl Volume {
    fn mount(dir: &Path, label: &str) -> Volume {
        Self::mount_fs(dir, label, "ExFAT")
    }
    /// `fs`: "ExFAT" (most SD cards) or "MS-DOS" (FAT32: cards of 32 GB and less).
    fn mount_fs(dir: &Path, label: &str, fs: &str) -> Volume {
        use std::process::Command;
        let image = dir.join(format!("{label}.dmg"));
        // Unique across tests running in parallel (FAT names: 11 chars max).
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("C{}{}{n}", &label[..1], std::process::id() % 100_000);
        let size = if fs == "MS-DOS" { "40m" } else { "16m" };
        assert!(Command::new("hdiutil").args(["create", "-quiet", "-size", size, "-fs", fs, "-volname", &name]).arg(&image).status().unwrap().success());
        let v = Volume { mount: format!("/Volumes/{name}"), image };
        v.attach();
        v
    }
    fn attach(&self) {
        assert!(std::process::Command::new("hdiutil").args(["attach", "-quiet", "-nobrowse"]).arg(&self.image).status().unwrap().success());
    }
    fn eject(&self) {
        assert!(std::process::Command::new("hdiutil").args(["detach", "-quiet", &self.mount]).status().unwrap().success());
    }
    /// Hash of every byte of the whole volume (eject first): file data, folders, FAT, timestamps.
    fn raw_hash(&self) -> u64 {
        xxhash_rust::xxh3::xxh3_64(&fs::read(&self.image).unwrap())
    }
    fn path(&self) -> &Path {
        Path::new(&self.mount)
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        if Path::new(&self.mount).exists() {
            let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet", "-force", &self.mount]).status();
        }
    }
}

/// The strongest check: every raw byte of the card is the same after a whole session (scan,
/// rescans, previews, decisions, export to another drive), on both card formats.
/// `cargo test -p cull-platform-fs --test source_safety -- --ignored`
#[test]
#[ignore]
fn a_whole_session_leaves_every_byte_of_a_real_card_unchanged() {
    for fs_kind in ["ExFAT", "MS-DOS"] {
        let tmp = tempfile::tempdir().unwrap();
        let card = Volume::mount_fs(tmp.path(), "card", fs_kind);
        make_card(card.path());
        card.eject();
        let before = card.raw_hash();
        card.attach();

        let drive = Volume::mount(tmp.path(), "drive");
        let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
        let src = svc.add_source(Arc::new(FsSource::new(card.path()).unwrap())).unwrap();
        svc.scan(&Default::default(), &NullSink).unwrap();
        svc.scan(&Default::default(), &NullSink).unwrap();
        svc.scan(&ScanRequest { source_ids: Some(vec![src.id.clone()]) }, &NullSink).unwrap();
        let q = svc.queue(&Filter::default()).unwrap();
        assert_eq!(q.len(), 4, "{fs_kind}");
        for p in &q {
            svc.preview(p.id, 512).unwrap();
            svc.decide(p.id, Decision::Like).unwrap();
        }
        let protect = || svc.known_source_ids().unwrap();
        assert!(svc.export(&CopyToDir::new(card.path().join("Keepers")).protecting(protect()), ExportOptions::default(), &NullSink).is_err());
        let r = svc.export(&CopyToDir::new(drive.path().join("Keepers")).protecting(protect()), ExportOptions::default(), &NullSink).unwrap();
        assert_eq!(r.copied, 4, "{fs_kind}");
        assert!(svc.clear_previews(&cull_platform_fs::is_external_and_connected).is_err(), "locked while the card is in");

        card.eject();
        assert_eq!(card.raw_hash(), before, "{fs_kind}: bytes on the card changed");
    }
}

/// Real exFAT volumes (what SD cards and most external SSDs use; no hard links, no
/// no-overwrite rename): export never writes onto the card, even next to the added folder,
/// and on an exFAT drive it still never overwrites and leaves no temp files or empty claims.
/// `cargo test -p cull-platform-fs --test source_safety -- --ignored`
#[test]
#[ignore]
fn export_on_real_exfat_volumes_never_touches_the_card_or_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let card = Volume::mount(tmp.path(), "card");
    let drive = Volume::mount(tmp.path(), "drive");
    make_card(card.path());
    let before = snapshot(card.path());

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(card.path().join("DCIM")).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    for p in svc.queue(&Filter::default()).unwrap() {
        svc.decide(p.id, Decision::Like).unwrap();
    }
    let export = |dest: &Path| svc.export(&CopyToDir::new(dest).protecting(svc.known_source_ids().unwrap()), ExportOptions::default(), &NullSink);

    // Next to the added folder, but on the card: refused.
    assert!(export(&card.path().join("Keepers")).is_err());
    assert_eq!(snapshot(card.path()), before, "the card is untouched");

    // Another exFAT drive: copies, then skips on a re-run.
    let out = drive.path().join("Keepers");
    assert_eq!(export(&out).unwrap().copied, 4);
    assert_eq!(export(&out).unwrap().already_there, 4);

    // A different file already using a keeper's name is kept; the keeper gets a suffix.
    let other = drive.path().join("Other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("DSC_0001.JPG"), b"someone else's file").unwrap();
    let r = export(&other).unwrap();
    assert_eq!((r.copied, r.failed.len()), (4, 0));
    assert_eq!(fs::read(other.join("DSC_0001.JPG")).unwrap(), b"someone else's file");
    assert!(other.join("DSC_0001-1.JPG").is_file());

    // No temp files or empty name claims left behind.
    for dir in [&out, &other] {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".partial") && e.metadata().unwrap().len() > 0, "leftover {name}");
        }
    }
    assert_eq!(snapshot(card.path()), before, "the card is untouched");
}
