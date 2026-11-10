//! The macOS app makes previews with ImageIO, not the portable code the main source-safety tests
//! use. Same guarantee here: the whole flow leaves the card byte-for-byte identical.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cull_core::*;
use cull_platform_apple::imageio::ImageIoPreviewMaker;
use cull_platform_fs::{CopyToDir, FsSource};
use image::{ImageBuffer, Rgb};

/// Every entry under `root` → (full contents, mtime, permissions).
fn snapshot(root: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime, u32)>) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        let m = fs::symlink_metadata(e.path()).unwrap();
        let bytes = if m.is_file() { fs::read(e.path()).unwrap() } else { Vec::new() };
        out.insert(e.path(), (bytes, m.modified().unwrap(), m.permissions().mode()));
        if m.is_dir() {
            snapshot(&e.path(), out);
        }
    }
}

#[test]
fn full_flow_with_imageio_previews_leaves_the_card_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    for (i, (w, h)) in [(3000, 2000), (2000, 3000), (800, 600)].into_iter().enumerate() {
        let path = card.join(format!("DCIM/DSC_000{i}.JPG"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        ImageBuffer::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 40 * i as u8])).save(&path).unwrap();
    }
    fs::write(card.join("DCIM/broken.JPG"), b"\xFF\xD8 not really a jpeg").unwrap();
    let mut before = BTreeMap::new();
    snapshot(&card, &mut before);

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.set_preview_maker(Arc::new(ImageIoPreviewMaker));
    let src = svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    svc.scan(&ScanRequest { source_ids: Some(vec![src.id.clone()]), force: true }, &NullSink).unwrap();
    for p in svc.queue(&Filter::default()).unwrap() {
        for edge in [256, 800, 2048] {
            let preview = svc.preview(p.id, edge);
            assert_eq!(preview.is_ok(), p.file_name != "broken.JPG", "{} at {edge}", p.file_name);
        }
        svc.decide(p.id, Decision::Like).unwrap();
    }
    let r = svc.export(&CopyToDir::new(tmp.path().join("out")).protecting(svc.known_source_ids().unwrap()), ExportOptions::default(), &NullSink).unwrap();
    assert_eq!(r.copied, 4);
    svc.clear_previews(&|_| false).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();

    let mut after = BTreeMap::new();
    snapshot(&card, &mut after);
    assert!(before == after, "the card changed");
}
