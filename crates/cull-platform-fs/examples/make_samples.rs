//! Generate fake "camera" JPEGs for trying the app without real photos.
//! Usage: cargo run --example make_samples -- <dir> [count]
//! Capture dates come from file mtimes (spread over 4 days), since these have no EXIF.

use image::{ImageBuffer, Rgb};
use std::time::{Duration, SystemTime};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: make_samples <dir> [count]"));
    let count: u32 = args.next().map(|c| c.parse().unwrap()).unwrap_or(80);
    let dcim = dir.join("DCIM/100SAMPL");
    std::fs::create_dir_all(&dcim).unwrap();
    let start = SystemTime::now() - Duration::from_secs(10 * 86_400);
    for i in 0..count {
        let (w, h) = if i % 5 == 0 { (2000, 3000) } else { (3000, 2000) };
        let hue = (i * 37 % 255) as u8;
        let img = ImageBuffer::from_fn(w, h, |x, y| {
            let cx = (x as i64 - (w as i64 / 2 + (i as i64 * 97 % 800) - 400)).abs();
            let cy = (y as i64 - h as i64 / 2).abs();
            let blob = if cx * cx + cy * cy < 300_000 { 120 } else { 0 };
            Rgb([hue.wrapping_add((x / 12) as u8), ((y / 12) as u8).wrapping_add(blob), 255 - hue])
        });
        let path = dcim.join(format!("DSC_{:04}.JPG", i + 1));
        img.save(&path).unwrap();
        // 4 shooting days, a few minutes apart within each day.
        let t = start + Duration::from_secs((i as u64 % 4) * 86_400 + 9 * 3600 + i as u64 * 180);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(t).unwrap();
    }
    println!("wrote {count} samples to {}", dcim.display());
}
