//! Where a preview's time goes: one photo rendered repeatedly by each preview maker, single-threaded.
//!
//!     cargo run --release -p cull-platform-apple --example preview_bench -- <photo.jpg>

use std::time::Instant;

use cull_core::ports::PreviewMaker;
use cull_core::preview::RustPreviewMaker;
use cull_platform_apple::imageio::ImageIoPreviewMaker;

fn main() {
    let path = std::env::args().nth(1).expect("usage: preview_bench <photo.jpg>");
    let bytes = std::fs::read(&path).unwrap();
    let makers: [&dyn PreviewMaker; 2] = [&ImageIoPreviewMaker, &RustPreviewMaker];
    for maker in makers {
        for edges in [&[2048, 256][..], &[2048], &[256]] {
            let n = 10;
            let t = Instant::now();
            let mut size = 0;
            for _ in 0..n {
                size = maker.render(&bytes, edges).unwrap().iter().map(Vec::len).sum::<usize>();
            }
            println!("{:<16} {:<12} {:>6.1} ms  ({} KB out)", maker.implementation(), format!("{edges:?}"), t.elapsed().as_secs_f64() * 1000.0 / n as f64, size / 1024);
        }
    }
}
