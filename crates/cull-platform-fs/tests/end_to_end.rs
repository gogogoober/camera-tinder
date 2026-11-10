//! Full M1 flow on real files: scan → cull → undo → export → rescan.

use std::sync::Arc;

use cull_core::*;
use cull_platform_fs::{CopyToDir, FsSource};
use image::{ImageBuffer, Rgb};

fn write_jpeg(path: &std::path::Path, w: u32, h: u32, shade: u8) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([shade, (x % 256) as u8, (y % 256) as u8]));
    img.save(path).unwrap();
}

#[test]
fn scan_cull_export_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    write_jpeg(&card.join("DCIM/100NIKON/DSC_0001.JPG"), 3000, 2000, 10);
    write_jpeg(&card.join("DCIM/100NIKON/DSC_0002.JPG"), 3000, 2000, 60);
    write_jpeg(&card.join("DCIM/100NIKON/DSC_0003.jpg"), 2000, 3000, 120);
    std::fs::write(card.join("DCIM/100NIKON/DSC_0003.NEF"), b"not really raw").unwrap();
    std::fs::write(card.join(".hidden.jpg"), b"junk").unwrap();
    std::fs::write(card.join("notes.txt"), b"hi").unwrap();

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();

    let report = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!(report.found, 3);
    assert_eq!(report.new, 3);
    assert_eq!(report.skipped_unsupported, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let queue = svc.queue(&Filter::default()).unwrap();
    assert_eq!(queue.len(), 3);

    // Preview: downscaled, valid JPEG, cached.
    let bytes = svc.preview(queue[0].id, 1024).unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    assert!(img.width() <= 1024 && img.height() <= 1024);
    assert_eq!(svc.preview(queue[0].id, 1024).unwrap(), bytes);

    svc.decide(queue[0].id, Decision::Like).unwrap();
    svc.decide(queue[1].id, Decision::Hate).unwrap();
    svc.decide(queue[2].id, Decision::Dislike).unwrap();
    let undone = svc.undo().unwrap();
    assert_eq!(undone.len(), 1);
    assert_eq!(undone[0].id, queue[2].id);
    assert_eq!(undone[0].decision, None);
    svc.decide(queue[2].id, Decision::Favorite).unwrap();

    let s = svc.stats().unwrap();
    assert_eq!((s.like, s.favorite, s.hate, s.undecided), (1, 1, 1, 0));

    // Export keepers; second export is a no-op.
    let dest = tmp.path().join("keepers");
    let target = CopyToDir::new(&dest);
    let r = svc.export(&target, ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 2, "{:?}", r.failed);
    let r2 = svc.export(&target, ExportOptions::default(), &NullSink).unwrap();
    assert_eq!((r2.copied, r2.already_there), (0, 2));
    // Originals untouched.
    assert!(card.join("DCIM/100NIKON/DSC_0002.JPG").exists());

    // Copy the card to "the laptop": decisions follow the photos.
    let laptop = tmp.path().join("laptop/Trip");
    std::fs::create_dir_all(&laptop).unwrap();
    for f in ["DSC_0001.JPG", "DSC_0002.JPG", "DSC_0003.jpg"] {
        std::fs::copy(card.join("DCIM/100NIKON").join(f), laptop.join(f)).unwrap();
    }
    let svc_id = card.canonicalize().unwrap().to_string_lossy().into_owned();
    svc.remove_source(&svc_id).unwrap();
    svc.add_source(Arc::new(FsSource::new(&laptop).unwrap())).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!((r.found, r.new), (3, 0));
    let s = svc.stats().unwrap();
    assert_eq!((s.total, s.undecided, s.favorite), (3, 0, 1));
}

#[test]
fn export_only_the_chosen_decisions_and_days_copies_the_originals_and_tags_persist() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    let names = ["DSC_0001.JPG", "DSC_0002.JPG", "DSC_0003.JPG", "DSC_0004.JPG"];
    for (i, n) in names.iter().enumerate() {
        write_jpeg(&card.join("DCIM/100NIKON").join(n), 800, 600, 20 + 50 * i as u8);
    }
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter::default()).unwrap();
    for (p, d) in q.iter().zip([Decision::Favorite, Decision::Like, Decision::Dislike, Decision::Hate]) {
        svc.decide(p.id, d).unwrap();
    }
    let q = svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    // Each photo keeps its folder and file name, so the original can always be found again.
    assert!(q.iter().all(|p| p.rel_path == format!("DCIM/100NIKON/{}", p.file_name)));

    let only = |likes, favs, nopes, hates| ExportOptions { include_likes: likes, include_favorites: favs, include_dislikes: nopes, include_hates: hates, include_exported: true, days: None };
    assert_eq!(svc.export_count(only(false, true, false, false)).unwrap(), 1);
    assert_eq!(svc.export_count(only(true, false, false, false)).unwrap(), 1);
    assert_eq!(svc.export_count(only(true, true, true, true)).unwrap(), 4);
    assert_eq!(svc.export_count(only(false, false, false, false)).unwrap(), 0);

    // Favorites only: exactly that original (full-size bytes, same name) lands in the folder.
    let dest = tmp.path().join("favs");
    let r = svc.export(&CopyToDir::new(&dest), only(false, true, false, false), &NullSink).unwrap();
    assert_eq!(r.copied, 1, "{:?}", r.failed);
    let fav = q.iter().find(|p| p.decision == Some(Decision::Favorite)).unwrap();
    let original = card.join(&fav.rel_path);
    let copy = std::fs::read_dir(&dest).unwrap().next().unwrap().unwrap().path();
    assert_eq!(copy.file_name().unwrap().to_string_lossy(), fav.file_name);
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&original).unwrap(), "the original, not a preview");

    // Days: a day nobody shot on selects nothing.
    let none = ExportOptions { days: Some(vec!["1999-01-01".into()]), ..only(true, true, true, true) };
    assert_eq!(svc.export_count(none).unwrap(), 0);

    // Tags: overlapping days, case-insensitive names, replaced days, deletion; nothing else touched.
    let japan = svc.save_tag("Japan trip", &["2026-03-02".into(), "2026-03-01".into(), "2026-03-01".into()]).unwrap();
    assert_eq!(japan.days, vec!["2026-03-01", "2026-03-02"]);
    let food = svc.save_tag("Food", &["2026-03-02".into(), "unknown".into()]).unwrap();
    assert_eq!(svc.list_tags().unwrap().len(), 2);
    let again = svc.save_tag("japan TRIP", &["2026-03-05".into()]).unwrap();
    assert_eq!((again.id, again.days), (japan.id, vec!["2026-03-05".to_string()]));
    assert!(svc.save_tag("  ", &[]).is_err());
    assert!(svc.save_tag("bad", &["tomorrow".into()]).is_err());
    svc.delete_tag(food.id).unwrap();
    assert_eq!(svc.list_tags().unwrap().len(), 1);
    assert_eq!(svc.stats().unwrap().favorite, 1);
}

#[test]
fn exported_photos_are_remembered_and_can_be_left_out_next_time() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    for (i, n) in ["A.JPG", "B.JPG", "C.JPG"].iter().enumerate() {
        write_jpeg(&card.join(n), 400, 300, 30 + 60 * i as u8);
    }
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let q = svc.queue(&Filter::default()).unwrap();
    svc.decide(q[0].id, Decision::Favorite).unwrap();
    svc.decide(q[1].id, Decision::Like).unwrap();
    let fresh = ExportOptions { include_exported: false, ..Default::default() };
    assert_eq!(svc.export_count(fresh.clone()).unwrap(), 2);

    let first = ExportOptions { include_exported: false, include_likes: false, ..Default::default() };
    let r = svc.export(&CopyToDir::new(tmp.path().join("one")), first, &NullSink).unwrap();
    assert_eq!(r.copied, 1);
    // The favourite is marked; leaving exported ones out leaves just the like, including them gives both.
    assert_eq!(svc.export_count(fresh.clone()).unwrap(), 1);
    assert_eq!(svc.export_count(ExportOptions::default()).unwrap(), 2);
    let r = svc.export(&CopyToDir::new(tmp.path().join("two")), fresh.clone(), &NullSink).unwrap();
    assert_eq!(r.copied, 1);
    assert_eq!(svc.export_count(fresh).unwrap(), 0, "everything has been exported");
    // Nothing about the photos' decisions changed.
    assert_eq!(svc.stats().unwrap().favorite, 1);
}
