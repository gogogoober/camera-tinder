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
    svc.remove_source(&svc_id);
    svc.add_source(Arc::new(FsSource::new(&laptop).unwrap())).unwrap();
    let r = svc.scan(&Default::default(), &NullSink).unwrap();
    assert_eq!((r.found, r.new), (3, 0));
    let s = svc.stats().unwrap();
    assert_eq!((s.total, s.undecided, s.favorite), (3, 0, 1));
}
