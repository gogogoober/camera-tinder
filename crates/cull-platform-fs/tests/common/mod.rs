//! Shared helpers for the offload tests: a test card with EXIF, an environment, snapshots.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cull_core::{CullerService, Library, MediaSource, NullSink};
use cull_platform_fs::{FsLibrary, FsSource};

// ---- test card -------------------------------------------------------------

/// A minimal EXIF APP1 segment: Make in IFD0, DateTimeOriginal in the Exif IFD.
pub fn exif_app1(make: &str, when: &str) -> Vec<u8> {
    let mut make = make.as_bytes().to_vec();
    make.push(0);
    if make.len() % 2 == 1 {
        make.push(0);
    }
    let mut dto = when.as_bytes().to_vec();
    dto.push(0);
    let entry = |t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
        t.extend(tag.to_le_bytes());
        t.extend(typ.to_le_bytes());
        t.extend(count.to_le_bytes());
        t.extend(value.to_le_bytes());
    };
    let ifd0 = 8u32;
    let make_off = ifd0 + 2 + 2 * 12 + 4;
    let exif_ifd = make_off + make.len() as u32;
    let dto_off = exif_ifd + 2 + 12 + 4;
    let mut t = b"II*\0".to_vec();
    t.extend(ifd0.to_le_bytes());
    t.extend(2u16.to_le_bytes());
    entry(&mut t, 0x010F, 2, make.len() as u32, make_off);
    entry(&mut t, 0x8769, 4, 1, exif_ifd);
    t.extend(0u32.to_le_bytes());
    t.extend(&make);
    t.extend(1u16.to_le_bytes());
    entry(&mut t, 0x9003, 2, dto.len() as u32, dto_off);
    t.extend(0u32.to_le_bytes());
    t.extend(&dto);
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend(((2 + 6 + t.len()) as u16).to_be_bytes());
    app1.extend(b"Exif\0\0");
    app1.extend(t);
    app1
}

/// A JPEG with EXIF; `seed` makes its pixels (and so its content) unique.
pub fn jpeg(make: &str, when: &str, seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([seed, (x * 4) as u8 ^ seed, (y * 5) as u8]));
    let mut plain = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Jpeg).unwrap();
    let mut out = plain[..2].to_vec();
    out.extend(exif_app1(make, when));
    out.extend(&plain[2..]);
    out
}

/// A Nikon NEF-like TIFF: the EXIF (Make, DateTimeOriginal) at the start, then `len` bytes of "sensor data".
pub fn nef(when: &str, seed: u8, len: usize) -> Vec<u8> {
    let app1 = exif_app1("NIKON CORPORATION", when);
    let mut out = app1[10..].to_vec(); // the TIFF inside "FF E1 len Exif\0\0"
    out.extend(noise(len, seed));
    out
}

/// Counts what is read from a source (to prove how much of the card a step touches).
pub struct CountingSource {
    pub inner: FsSource,
    pub bytes: Arc<std::sync::atomic::AtomicU64>,
}
struct CountingReader {
    r: Box<dyn cull_core::ReadSeek>,
    n: Arc<std::sync::atomic::AtomicU64>,
}
impl std::io::Read for CountingReader {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let k = self.r.read(b)?;
        self.n.fetch_add(k as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(k)
    }
}
impl std::io::Seek for CountingReader {
    fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
        self.r.seek(p)
    }
}
impl MediaSource for CountingSource {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn list(&self) -> cull_core::Result<Vec<cull_core::MediaEntry>> {
        self.inner.list()
    }
    fn list_complete(&self) -> cull_core::Result<Vec<cull_core::MediaEntry>> {
        self.inner.list_complete()
    }
    fn open(&self, r: &str) -> cull_core::Result<Box<dyn cull_core::ReadSeek>> {
        Ok(Box::new(CountingReader { r: self.inner.open(r)?, n: self.bytes.clone() }))
    }
    fn open_streaming(&self, r: &str) -> cull_core::Result<Box<dyn cull_core::ReadSeek>> {
        Ok(Box::new(CountingReader { r: self.inner.open_streaming(r)?, n: self.bytes.clone() }))
    }
}

/// A Fuji RAF: header, then the embedded JPEG (with its EXIF), then "sensor data".
pub fn raf(when: &str, seed: u8) -> Vec<u8> {
    let embedded = jpeg("FUJIFILM", when, seed);
    let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    out.resize(84, 0);
    out.extend(160u32.to_be_bytes());
    out.extend((embedded.len() as u32).to_be_bytes());
    out.resize(160, 0);
    out.extend(&embedded);
    out.extend((0..300_000u32).map(|i| (i.wrapping_mul(7) as u8) ^ seed));
    out
}

pub fn noise(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 13) as u8 ^ seed).collect()
}

pub fn put(path: &Path, bytes: &[u8], mtime: Option<&str>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    if let Some(m) = mtime {
        let t = chrono::NaiveDateTime::parse_from_str(m, "%Y-%m-%dT%H:%M:%S").unwrap().and_local_timezone(chrono::Local).unwrap();
        fs::File::options().write(true).open(path).unwrap().set_modified(t.into()).unwrap();
    }
}

pub fn make_card(card: &Path) {
    let f = card.join("DCIM/100_FUJI");
    let n = card.join("DCIM/100NZ6_2");
    put(&f.join("DSCF0001.JPG"), &jpeg("FUJIFILM", "2026:10:05 10:00:00", 1), None);
    put(&f.join("DSCF0001.RAF"), &raf("2026:10:05 10:00:00", 1), None);
    put(&f.join("DSCF0002.MOV"), &noise(500_000, 2), Some("2026-10-05T12:00:00"));
    put(&f.join("DSCF0002.LRF"), &noise(1000, 3), Some("2026-10-05T12:00:00"));
    put(&n.join("DSC_0001.JPG"), &jpeg("NIKON CORPORATION", "2026:11:02 01:30:00", 4), None); // before 6 a.m.: Nov 1
    put(&n.join("DSC_0002.NEF"), &noise(200_000, 5), Some("2026-11-03T15:00:00"));
}

pub const EXPECTED: &[&str] = &[
    "Photos/Fuji/2026/Japan trip/DSCF0001.JPG",
    "Photos/Fuji/2026/Japan trip/RAW/DSCF0001.RAF",
    "Photos/Fuji/2026/Japan trip/Videos/DSCF0002.MOV",
    "Photos/Nikon/2026/2026-11/DSC_0001.JPG",
    "Photos/Nikon/2026/2026-11/RAW/DSC_0002.NEF",
];

// ---- helpers ---------------------------------------------------------------

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub card: PathBuf,
    pub ssd: PathBuf,
    pub svc: Arc<CullerService>,
}

pub fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    let ssd = tmp.path().join("ssd");
    fs::create_dir_all(&ssd).unwrap();
    make_card(&card);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    let days: Vec<String> = (1..=14).map(|d| format!("2026-10-{d:02}")).collect();
    svc.save_tag("Japan trip", &days).unwrap();
    Env { card, ssd, svc, _tmp: tmp }
}

pub fn plan_full(e: &Env, lib: Arc<dyn Library>, full_check: bool) -> cull_core::offload::OffloadPlan {
    try_plan(e, Arc::new(FsSource::new(&e.card).unwrap()), lib, full_check).unwrap()
}

pub fn try_plan(e: &Env, card: Arc<dyn MediaSource>, lib: Arc<dyn Library>, full_check: bool) -> cull_core::Result<cull_core::offload::OffloadPlan> {
    e.svc.offload_plan(card, e.card.to_string_lossy().into_owned(), Arc::new(FsSource::new(&e.ssd).unwrap()), lib, full_check, &NullSink)
}

pub fn plan_with(e: &Env, lib: Arc<dyn Library>) -> cull_core::offload::OffloadPlan {
    plan_full(e, lib, false)
}

pub fn fs_lib(e: &Env) -> Arc<dyn Library> {
    Arc::new(FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap())
}

pub fn plan(e: &Env) -> cull_core::offload::OffloadPlan {
    plan_with(e, Arc::new(FsLibrary::allowing_same_volume_for_tests(&e.ssd, &e.card).unwrap()))
}

pub fn start(e: &Env) -> cull_core::offload::OffloadReport {
    e.svc.offload_start(&[], false, &NullSink).unwrap()
}

/// Every file: relative path → bytes hash, size, mtime.
pub fn snapshot(root: &Path) -> BTreeMap<String, (u64, u64, std::time::SystemTime)> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let m = e.metadata().unwrap();
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().into_owned();
            (rel, (xxhash_rust::xxh3::xxh3_64(&fs::read(e.path()).unwrap()), m.len(), m.modified().unwrap()))
        })
        .collect()
}

pub fn partials(root: &Path) -> Vec<String> {
    snapshot(root).into_keys().filter(|k| k.ends_with(".partial")).collect()
}

pub fn photo_files(root: &Path) -> Vec<String> {
    snapshot(root).into_keys().filter(|k| !k.contains("_darkroom")).collect()
}

