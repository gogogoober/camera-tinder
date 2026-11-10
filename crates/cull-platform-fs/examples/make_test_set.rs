//! Build the ~100-photo test set (`test-photos-v2/`): real photos + stock + a few synthetic ones, with
//! believable dates over six weeks and an answer key (`manifest.json`).
//!
//! Usage: cargo run --release -p cull-platform-fs --example make_test_set -- <stock_dir> <source_dir> <out_dir>
//!   stock_dir  = the old Lorem Picsum photos (any folder of JPEGs); some are used as-is, some as bases
//!   source_dir = test-photos-v2/source (hair-flip, dancer-beach, dress-road, motorbike, portrait-focal, accidental)
//!   out_dir    = e.g. test-photos-v2/set (writes DCIM/100TEST/DSC_0001.JPG… and manifest.json)
//! Capture times come from file mtimes (the scan falls back to them when there is no EXIF).
//! Everything is deterministic: the same inputs give the same set.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{Duration, Local, NaiveDateTime, TimeZone};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{Rgb, RgbImage};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

enum Pixels {
    Copy(PathBuf),
    Image(RgbImage),
}

struct Item {
    time: NaiveDateTime,
    pixels: Pixels,
    source: String,
    synthetic: bool,
    /// sweep = should be set aside; judge = a normal single photo; group = belongs to `group`.
    expect: &'static str,
    group: Option<String>,
    note: String,
}

fn t(day: &str, hm: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(&format!("2026-{day} {hm}:00"), "%Y-%m-%d %H:%M:%S").unwrap()
}

fn load(p: &Path) -> RgbImage {
    image::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())).to_rgb8()
}

// ---- synthetic transforms ------------------------------------------------

/// One frame of a handheld burst: tiny zoom/shift, small exposure change. Frame 0 is the original.
fn burst_frame(base: &RgbImage, i: usize, rng: &mut Rng) -> RgbImage {
    if i == 0 {
        return base.clone();
    }
    let (w, h) = base.dimensions();
    let z = 1.0 + rng.unit() * 0.03;
    let (cw, ch) = (((w as f32) / z) as u32, ((h as f32) / z) as u32);
    let x0 = rng.range(0, (w - cw) as i64) as u32;
    let y0 = rng.range(0, (h - ch) as i64) as u32;
    let crop = imageops::crop_imm(base, x0, y0, cw, ch).to_image();
    let mut out = imageops::resize(&crop, w, h, FilterType::Triangle);
    imageops::colorops::brighten_in_place(&mut out, rng.range(-5, 5) as i32);
    if i == 3 {
        out = imageops::blur(&out, 1.0); // one slightly soft frame, as in a real burst
    }
    out
}

fn small(img: &RgbImage, w: u32) -> RgbImage {
    let h = (img.height() as f32 * w as f32 / img.width() as f32) as u32;
    imageops::resize(img, w, h, FilterType::Triangle)
}

fn heavy_blur(img: &RgbImage) -> RgbImage {
    let s = small(img, 800);
    let b = imageops::blur(&s, 14.0);
    imageops::resize(&b, img.width(), img.height(), FilterType::Triangle)
}

fn motion_blur(img: &RgbImage) -> RgbImage {
    let s = small(img, 1200);
    let (w, h) = s.dimensions();
    let r = 38i64;
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w as i64 {
            let mut acc = [0u32; 3];
            let mut n = 0;
            for dx in -r..=r {
                let xx = (x + dx).clamp(0, w as i64 - 1) as u32;
                let p = s.get_pixel(xx, y);
                for c in 0..3 {
                    acc[c] += p[c] as u32;
                }
                n += 1;
            }
            out.put_pixel(x as u32, y, Rgb([(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]));
        }
    }
    imageops::resize(&out, img.width(), img.height(), FilterType::Triangle)
}

fn whitewash(img: &RgbImage) -> RgbImage {
    let mut o = img.clone();
    for p in o.pixels_mut() {
        for c in 0..3 {
            p[c] = (255.0 - (255.0 - p[c] as f32) * 0.03) as u8;
        }
    }
    o
}

fn finger(img: &RgbImage) -> RgbImage {
    let mut o = img.clone();
    let (w, h) = (o.width() as f32, o.height() as f32);
    let (cx, cy, rx, ry) = (w * 0.2, h * 0.98, w * 0.42, h * 0.5);
    for (x, y, p) in o.enumerate_pixels_mut() {
        let d = ((x as f32 - cx) / rx).powi(2) + ((y as f32 - cy) / ry).powi(2);
        if d < 1.0 {
            let k = (1.0 - d).sqrt().min(1.0);
            let skin = [150.0, 80.0, 70.0];
            for c in 0..3 {
                p[c] = (p[c] as f32 * (1.0 - k) + skin[c] * k) as u8;
            }
        }
    }
    o
}

fn flat(w: u32, h: u32, base: [u8; 3], noise: i64, rng: &mut Rng) -> RgbImage {
    RgbImage::from_fn(w, h, |_, _| {
        let n = rng.range(-noise, noise);
        Rgb([0, 1, 2].map(|c| (base[c] as i64 + n).clamp(0, 255) as u8))
    })
}

fn low_key(img: &RgbImage) -> RgbImage {
    let mut o = img.clone();
    for p in o.pixels_mut() {
        for c in 0..3 {
            p[c] = (p[c] as f32 * 0.32) as u8;
        }
    }
    o
}

fn high_key(img: &RgbImage) -> RgbImage {
    let mut o = img.clone();
    for p in o.pixels_mut() {
        for c in 0..3 {
            p[c] = (255.0 - (255.0 - p[c] as f32) * 0.42) as u8;
        }
    }
    o
}

// ---- the set -------------------------------------------------------------

fn main() {
    let mut args = std::env::args().skip(1);
    let stock_dir = PathBuf::from(args.next().expect("usage: make_test_set <stock_dir> <source_dir> <out_dir>"));
    let src = PathBuf::from(args.next().expect("source_dir"));
    let out = PathBuf::from(args.next().expect("out_dir"));
    let mut rng = Rng(0x9E3779B97F4A7C15);

    let mut stock: Vec<PathBuf> = std::fs::read_dir(&stock_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg")))
        .collect();
    stock.sort();
    let need = 43;
    assert!(stock.len() >= need, "need {need} stock photos, found {}", stock.len());
    // Evenly spaced picks so the set isn't just one corner of the stock.
    let mut picks: Vec<PathBuf> = (0..need).map(|i| stock[i * stock.len() / need].clone()).collect();
    picks.reverse(); // pop() takes them in order
    let mut next_stock = move || picks.pop().expect("out of stock photos");

    let mut items: Vec<Item> = Vec::new();
    let real = |dir: &str, name: &str| src.join(dir).join(name);

    // A real photo copied as-is.
    let add_real = |items: &mut Vec<Item>, time: NaiveDateTime, path: PathBuf, expect: &'static str, group: Option<&str>, note: &str| {
        items.push(Item { time, source: path.display().to_string(), pixels: Pixels::Copy(path), synthetic: false, expect, group: group.map(String::from), note: note.into() });
    };

    // Filler: ordinary stock photos spread through a session.
    let filler = |items: &mut Vec<Item>, rng: &mut Rng, next: &mut dyn FnMut() -> PathBuf, start: NaiveDateTime, n: usize| {
        let mut at = start;
        for _ in 0..n {
            let p = next();
            items.push(Item { time: at, source: p.display().to_string(), pixels: Pixels::Copy(p), synthetic: false, expect: "judge", group: None, note: "ordinary stock photo".into() });
            at += Duration::seconds(rng.range(120, 900));
        }
    };

    // Oct 3: beach dancer (the same scene, poses differ: a similar group, not a burst).
    let mut at = t("10-03", "10:12");
    for (i, gap) in [0, 45, 70, 38].iter().enumerate() {
        at += Duration::seconds(*gap);
        add_real(&mut items, at, real("dancer-beach", &format!("dancer_{:02}.jpg", i + 1)), "judge", None, "real; same scene, different poses");
    }
    filler(&mut items, &mut rng, &mut next_stock, t("10-03", "10:40"), 3);

    // Oct 4: dress on the road, then the motorbike.
    let mut at = t("10-04", "14:05");
    for (i, gap) in [0, 52, 33].iter().enumerate() {
        at += Duration::seconds(*gap);
        add_real(&mut items, at, real("dress-road", &format!("dress_{:02}.jpg", i + 1)), "judge", None, "real; moving subject, different poses");
    }
    let mut at = t("10-04", "14:30");
    for (i, gap) in [0, 25, 31, 19, 40, 28].iter().enumerate() {
        at += Duration::seconds(*gap);
        add_real(&mut items, at, real("motorbike", &format!("motorbike_{:02}.jpg", i + 1)), "judge", None, "real; deliberate panning blur: must not be swept");
    }

    // Oct 10: hair-flip shoot. Frames 1-2, 3-4, 7-8, 9-10, 11-12, 13-14 are real near-identical pairs, 1 s apart.
    let gaps = [0, 1, 15, 1, 20, 12, 18, 1, 16, 1, 14, 1, 17, 1, 19];
    let pair_of = |n: usize| match n {
        1 | 2 => Some("hairflip-a"),
        3 | 4 => Some("hairflip-b"),
        7 | 8 => Some("hairflip-c"),
        9 | 10 => Some("hairflip-d"),
        11 | 12 => Some("hairflip-e"),
        13 | 14 => Some("hairflip-f"),
        _ => None,
    };
    let mut at = t("10-10", "11:00");
    for (i, gap) in gaps.iter().enumerate() {
        at += Duration::seconds(*gap);
        let n = i + 1;
        let g = pair_of(n);
        let (expect, note) = if g.is_some() { ("group", "real near-identical pair, 1 s apart") } else { ("judge", "real; high-key black and white on white: must not be swept") };
        add_real(&mut items, at, real("hair-flip", &format!("hairflip_{n:02}.jpg")), expect, g, note);
    }

    // Burst A (Oct 11): six near-identical frames, 1 s apart, from one stock photo.
    let burst = |items: &mut Vec<Item>, rng: &mut Rng, base_path: PathBuf, start: NaiveDateTime, id: &str| {
        let base = load(&base_path);
        for i in 0..6 {
            let f = burst_frame(&base, i, rng);
            items.push(Item {
                time: start + Duration::seconds(i as i64),
                pixels: Pixels::Image(f),
                source: format!("{} (frame {})", base_path.display(), i + 1),
                synthetic: i > 0,
                expect: "group",
                group: Some(id.into()),
                note: "burst: tiny shifts, exposure changes (synthetic except frame 1)".into(),
            });
        }
    };
    burst(&mut items, &mut rng, next_stock(), t("10-11", "16:20"), "burst-a");
    filler(&mut items, &mut rng, &mut next_stock, t("10-11", "17:05"), 2);

    // Oct 17: portrait at different focal lengths, 35-60 s apart.
    let mut at = t("10-17", "09:30");
    for (i, gap) in [0, 35, 41, 28, 60, 33, 47, 52].iter().enumerate() {
        at += Duration::seconds(*gap);
        add_real(&mut items, at, real("portrait-focal", &format!("portrait_{:02}.jpg", i + 1)), "judge", None, "real; same pose, different focal lengths (not a burst); has text overlays");
    }

    // Unrelated shots seconds apart: must NOT be grouped.
    let pair = |items: &mut Vec<Item>, next: &mut dyn FnMut() -> PathBuf, start: NaiveDateTime, gap: i64| {
        for k in 0..2 {
            let p = next();
            items.push(Item { time: start + Duration::seconds(k * gap), source: p.display().to_string(), pixels: Pixels::Copy(p), synthetic: false, expect: "judge", group: None, note: "unrelated to its neighbour, taken seconds apart: must not group".into() });
        }
    };
    pair(&mut items, &mut next_stock, t("10-18", "13:00"), 2);
    filler(&mut items, &mut rng, &mut next_stock, t("10-18", "13:20"), 2);

    // Oct 20: real pocket shot between ordinary photos.
    filler(&mut items, &mut rng, &mut next_stock, t("10-20", "18:45"), 2);
    add_real(&mut items, t("10-20", "19:20"), src.join("accidental/accidental_black_pocket.jpg"), "sweep", None, "real accidental: pocket / all black");
    filler(&mut items, &mut rng, &mut next_stock, t("10-20", "19:40"), 1);

    // Oct 24: burst B.
    burst(&mut items, &mut rng, next_stock(), t("10-24", "11:10"), "burst-b");
    filler(&mut items, &mut rng, &mut next_stock, t("10-24", "12:00"), 3);

    // Oct 25: unrelated pair + synthetic lens cap.
    pair(&mut items, &mut next_stock, t("10-25", "15:00"), 1);
    items.push(Item { time: t("10-25", "15:01"), pixels: Pixels::Image(flat(3000, 2000, [4, 4, 5], 5, &mut rng)), source: "generated".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic accidental: lens cap / all black".into() });

    // Oct 26: real floor shot.
    add_real(&mut items, t("10-26", "08:15"), src.join("accidental/accidental_floor_foot.jpg"), "judge", None, "real accidental but sharp and well exposed: hard to catch, should not auto-sweep");
    filler(&mut items, &mut rng, &mut next_stock, t("10-26", "08:20"), 2);

    // Nov 1: burst C + heavy blur.
    burst(&mut items, &mut rng, next_stock(), t("11-01", "17:30"), "burst-c");
    let base = load(&next_stock());
    items.push(Item { time: t("11-01", "17:31"), pixels: Pixels::Image(heavy_blur(&base)), source: "stock photo, heavily blurred".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic: blurred all over, subject unrecognisable".into() });

    // Nov 2: unrelated pair + risky keepers.
    pair(&mut items, &mut next_stock, t("11-02", "12:00"), 2);
    let base = load(&next_stock());
    items.push(Item { time: t("11-02", "12:30"), pixels: Pixels::Image(low_key(&base)), source: "stock photo, darkened".into(), synthetic: true, expect: "judge", group: None, note: "synthetic intentional low-key: must not be swept".into() });
    let base = load(&next_stock());
    items.push(Item { time: t("11-02", "12:45"), pixels: Pixels::Image(high_key(&base)), source: "stock photo, brightened".into(), synthetic: true, expect: "judge", group: None, note: "synthetic intentional high-key: must not be swept".into() });

    // Nov 3: real lens-covered shot + near-white frame.
    add_real(&mut items, t("11-03", "10:05"), src.join("accidental/accidental_lens_covered_pink.jpg"), "sweep", None, "real accidental: lens covered");
    filler(&mut items, &mut rng, &mut next_stock, t("11-03", "10:30"), 2);
    let base = load(&next_stock());
    items.push(Item { time: t("11-03", "11:10"), pixels: Pixels::Image(whitewash(&base)), source: "stock photo, washed out".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic accidental: blown out to white".into() });

    // Nov 6: real bed blur + synthetic finger, blank wall, motion blur.
    add_real(&mut items, t("11-06", "20:10"), src.join("accidental/accidental_motion_blur_bed.jpg"), "sweep", None, "real accidental: camera swung, blurred all over");
    filler(&mut items, &mut rng, &mut next_stock, t("11-06", "20:30"), 2);
    let base = load(&next_stock());
    items.push(Item { time: t("11-06", "21:00"), pixels: Pixels::Image(finger(&base)), source: "stock photo with a finger over the lens".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic accidental: finger covers a corner (the rest is sharp)".into() });
    items.push(Item { time: t("11-06", "21:02"), pixels: Pixels::Image(flat(3000, 2000, [205, 203, 196], 2, &mut rng)), source: "generated".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic accidental: blank wall".into() });
    let base = load(&next_stock());
    items.push(Item { time: t("11-06", "21:05"), pixels: Pixels::Image(motion_blur(&base)), source: "stock photo, horizontal smear".into(), synthetic: true, expect: "sweep", group: None, note: "synthetic accidental: camera shake smear".into() });

    // Nov 8: the rest of the ordinary stock photos, plus two more risky keepers from stock.
    filler(&mut items, &mut rng, &mut next_stock, t("11-08", "10:00"), 7);
    let base = load(&next_stock());
    items.push(Item { time: t("11-08", "19:00"), pixels: Pixels::Image(low_key(&base)), source: "stock photo, darkened".into(), synthetic: true, expect: "judge", group: None, note: "synthetic intentional low-key (dusk): must not be swept".into() });
    let base = load(&next_stock());
    items.push(Item { time: t("11-08", "19:10"), pixels: Pixels::Image(high_key(&base)), source: "stock photo, brightened".into(), synthetic: true, expect: "judge", group: None, note: "synthetic intentional high-key: must not be swept".into() });

    // ---- write ---------------------------------------------------------
    items.sort_by_key(|i| i.time);
    let dcim = out.join("DCIM/100TEST");
    std::fs::create_dir_all(&dcim).unwrap();
    let mut manifest = String::from("[\n");
    for (n, item) in items.iter().enumerate() {
        let name = format!("DSC_{:04}.JPG", n + 1);
        let path = dcim.join(&name);
        match &item.pixels {
            Pixels::Copy(p) => {
                std::fs::copy(p, &path).unwrap();
            }
            Pixels::Image(img) => {
                let mut f = std::fs::File::create(&path).unwrap();
                JpegEncoder::new_with_quality(&mut f, 92).encode_image(img).unwrap();
            }
        }
        let when: SystemTime = Local.from_local_datetime(&item.time).single().expect("local time").into();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(when).unwrap();
        manifest += &format!(
            "  {{\"file\": \"{name}\", \"time\": \"{}\", \"expect\": \"{}\", \"group\": {}, \"synthetic\": {}, \"source\": {:?}, \"note\": {:?}}}{}\n",
            item.time.format("%Y-%m-%dT%H:%M:%S"),
            item.expect,
            item.group.as_ref().map(|g| format!("{g:?}")).unwrap_or("null".into()),
            item.synthetic,
            item.source,
            item.note,
            if n + 1 < items.len() { "," } else { "" }
        );
    }
    manifest += "]\n";
    std::fs::write(out.join("manifest.json"), manifest).unwrap();
    println!("wrote {} photos to {}", items.len(), dcim.display());
}
