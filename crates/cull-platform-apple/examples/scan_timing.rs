//! Time a fresh scan of a folder (empty database, empty preview cache), the way the app scans.
//!
//!     cargo run --release -p cull-platform-apple --example scan_timing -- <dir> [--rust]
//!
//! Uses the ImageIO preview maker (what the macOS app uses); `--rust` uses the portable one.

use std::sync::Arc;
use std::time::Instant;

use cull_core::CullerService;
use cull_core::ports::NullSink;
use cull_platform_fs::FsSource;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: scan_timing <dir>");
    let tmp = tempfile::tempdir().unwrap();
    let svc = CullerService::in_memory(&tmp.path().join("previews")).unwrap();
    if !std::env::args().any(|a| a == "--rust") {
        svc.set_preview_maker(Arc::new(cull_platform_apple::imageio::ImageIoPreviewMaker));
    }
    svc.add_source(Arc::new(FsSource::new(&dir).unwrap())).unwrap();

    let t = Instant::now();
    let report = svc.scan(&Default::default(), &NullSink).unwrap();
    let secs = t.elapsed().as_secs_f64();
    println!("{} photos ({} new, {} failed) in {secs:.2}s = {:.0} ms/photo", report.found, report.new, report.failed.len(), 1000.0 * secs / report.found.max(1) as f64);

    let t = Instant::now();
    svc.scan(&Default::default(), &NullSink).unwrap();
    println!("rescan (nothing new): {:.4}s", t.elapsed().as_secs_f64());

    let (files, bytes) = cull_core::preview::PreviewCache::new(tmp.path().join("previews")).unwrap().usage().unwrap();
    println!("preview cache: {files} files, {:.0} KB per photo", bytes as f64 / 1024.0 / report.found.max(1) as f64);
}
