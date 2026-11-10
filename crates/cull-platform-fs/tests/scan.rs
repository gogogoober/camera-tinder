//! Scan behaviour: rescans read only new files, saves happen in batches, and Stop keeps the work done.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use cull_core::*;
use cull_platform_fs::FsSource;
use image::{ImageBuffer, Rgb};

fn write_jpeg(path: &Path, shade: u8) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    ImageBuffer::from_fn(96, 64, |x, y| Rgb([shade, (x * 2) as u8, (y * 3) as u8])).save(path).unwrap();
}

fn card(root: &Path, n: u8) {
    for i in 0..n {
        write_jpeg(&root.join(format!("DCIM/DSC_{i:04}.JPG")), i);
    }
}

#[test]
fn rescans_read_only_new_files_and_requests_can_name_one_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 3);
    std::fs::write(root.join("DCIM/DSC_0000.NEF"), b"raw").unwrap();
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    let src = svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();

    let r = svc.scan(&ScanRequest::default(), &NullSink).unwrap();
    assert_eq!((r.found, r.new, r.skipped_unsupported), (3, 3, 1));

    // Same files: everything is found again, nothing is new.
    let r = svc.scan(&ScanRequest::default(), &NullSink).unwrap();
    assert_eq!((r.found, r.new, r.skipped_unsupported), (3, 0, 1));

    // A new photo: only that one is new.
    write_jpeg(&root.join("DCIM/DSC_0099.JPG"), 99);
    let r = svc.scan(&ScanRequest { source_ids: Some(vec![src.id.clone()]) }, &NullSink).unwrap();
    assert_eq!((r.found, r.new), (4, 1));

    // Asking for another folder leaves this one alone.
    let r = svc.scan(&ScanRequest { source_ids: Some(vec!["/elsewhere".into()]) }, &NullSink).unwrap();
    assert_eq!(r.found, 0);
}

/// Stops the scan as soon as the first batch of photos is saved.
struct StopAfterFirstSave {
    svc: OnceLock<Arc<CullerService>>,
    saved: Mutex<Vec<u64>>,
}

impl EventSink for StopAfterFirstSave {
    fn emit(&self, event: CoreEvent) {
        if let CoreEvent::ScanSaved { saved, .. } = event {
            self.saved.lock().unwrap().push(saved);
            self.svc.get().unwrap().cancel_scan();
        }
    }
}

#[test]
fn stopping_keeps_saved_batches_and_the_next_scan_finishes_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("card");
    card(&root, 40);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    svc.add_source(Arc::new(FsSource::new(&root).unwrap())).unwrap();

    let sink = StopAfterFirstSave { svc: OnceLock::new(), saved: Mutex::new(Vec::new()) };
    sink.svc.set(svc.clone()).ok();
    let r = svc.scan(&ScanRequest::default(), &sink).unwrap();
    assert!(r.cancelled);
    assert_eq!(*sink.saved.lock().unwrap(), vec![32], "one batch saved, then stopped");
    assert_eq!(svc.queue(&Filter::default()).unwrap().len(), 32, "saved photos are visible right away");

    // A stopped scan doesn't mark the folder as done, so a plain rescan finishes it.
    let r = svc.scan(&ScanRequest::default(), &NullSink).unwrap();
    assert!(!r.cancelled);
    assert_eq!(r.new, 8);
    assert_eq!(svc.queue(&Filter::default()).unwrap().len(), 40);
}
