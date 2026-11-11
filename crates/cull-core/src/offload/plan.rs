//! Planning an offload: read the card, find what the backup drive already has (by content), and lay
//! the rest out in folders. `gather` does the reading; `layout` is pure and re-runs on every folder choice.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use super::journal::{BrandBackup, CachedFile};
use super::kind::{FileKind, extension};
use super::layout::{FolderKey, Span, broadest, sanitize};
use super::meta::{brand_from_card_folder, brand_from_file_name, brand_from_make, read_file_meta};
use crate::days::photo_day;
use crate::error::{CoreError, Result};
use crate::identity::fingerprint;
use crate::ports::{CalendarSource, CoreEvent, EventSink, Library, LibrarySpace, MediaSource};
use crate::store::Store;

/// Our own files on the drive live here, inside the layout root.
pub const OWN_DIR: &str = "_darkroom";
pub const LIBRARY_ID_FILE: &str = "_darkroom/library-id";
pub const UNKNOWN_BRAND: &str = "Unknown camera";

/// A photo or video on the card.
#[derive(Debug, Clone)]
pub struct CardFile {
    pub rel_path: String,
    pub size: u64,
    pub kind: FileKind,
    pub fingerprint: String,
    pub captured_at: Option<NaiveDateTime>,
    pub brand: String,
    /// Already on the drive (same content), at this path.
    pub on_drive: Option<String>,
}

impl CardFile {
    pub fn day(&self) -> Option<NaiveDate> {
        self.captured_at.map(photo_day)
    }
}

/// Everything read from the card and the drive; `layout` turns it into a plan without reading again.
#[derive(Debug, Clone)]
pub struct PlanInput {
    pub card_root: String,
    pub library_root: String,
    /// "Photos/" (or "" when the picked folder is itself called Photos): where new folders go.
    pub prefix: String,
    pub library_id: Option<String>,
    /// Key of the drive's fingerprint cache: the library id, or the picked path before the drive has one.
    pub cache_key: String,
    pub files: Vec<CardFile>,
    /// Card files that are not photos or videos, by extension: never copied.
    pub skipped: BTreeMap<String, u64>,
    /// Card photos/videos that couldn't be read while planning: (path, error).
    pub unreadable: Vec<(String, String)>,
    pub spans: Vec<Span>,
    /// Every file and folder on the drive, lower-cased (APFS and exFAT ignore case).
    pub existing_files: HashSet<String>,
    pub existing_dirs: HashSet<String>,
    /// Folders made by earlier offloads: (brand, year, key) → path.
    pub remembered: HashMap<(String, i32, String), String>,
    pub space: LibrarySpace,
    pub last_backups: Vec<BrandBackup>,
    /// "Already on the drive" was confirmed byte for byte (not just by fingerprint).
    pub full_check: bool,
}

/// The user's choice for one folder of the plan.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderChoice {
    /// `PlanFolder::id`
    pub id: String,
    /// Typed or picked folder name.
    #[serde(default)]
    pub name: Option<String>,
    /// For a trip folder: use month folders instead.
    #[serde(default)]
    pub by_month: bool,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    /// Index into `PlanInput::files`.
    pub index: usize,
    pub dest_rel: String,
    pub brand: String,
    pub year: i32,
    pub folder_key: String,
    pub folder_rel: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFolder {
    pub id: String,
    pub brand: String,
    pub year: Option<i32>,
    pub name: String,
    /// Relative to the picked folder.
    pub path: String,
    pub is_trip: bool,
    /// Already on the drive (new files are added next to what's there).
    pub exists: bool,
    pub files: u64,
    pub bytes: u64,
    pub jpeg: u64,
    pub raw: u64,
    pub heif: u64,
    pub video: u64,
    pub first_day: Option<String>,
    pub last_day: Option<String>,
    /// Other names that fit (tags covering these days), broadest first.
    pub options: Vec<String>,
    /// Trip folders (ids and names) whose photos are here because they were split by month.
    pub split_from: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountByType {
    pub extension: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedFile {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OffloadPlan {
    pub card_root: String,
    pub library_root: String,
    pub new_files: u64,
    pub new_bytes: u64,
    pub already_files: u64,
    pub already_bytes: u64,
    pub skipped: Vec<CountByType>,
    pub unreadable: Vec<FailedFile>,
    pub folders: Vec<PlanFolder>,
    pub free_bytes: u64,
    pub fs_type: String,
    pub warnings: Vec<String>,
    pub last_backups: Vec<BrandBackup>,
    /// "Already on the drive" was confirmed byte for byte (not just by fingerprint).
    pub full_check: bool,
}

/// Bytes kept free on the drive beyond the plan.
pub const SPACE_MARGIN: u64 = 1 << 30;

fn local_naive(t: SystemTime) -> NaiveDateTime {
    DateTime::<Local>::from(t).naive_local()
}

fn millis(t: Option<SystemTime>) -> Option<i64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where new folders go inside the picked folder: `Photos/`, unless the picked folder is called Photos.
pub fn prefix_for(library_root: &str) -> String {
    let last = library_root.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if last.eq_ignore_ascii_case("photos") { String::new() } else { "Photos/".into() }
}

pub struct Gather<'a> {
    pub card: &'a dyn MediaSource,
    pub card_root: String,
    /// Reads the drive (listing + fingerprints of candidate matches), rooted at the picked folder.
    pub library: &'a dyn MediaSource,
    pub lib: &'a dyn Library,
    pub store: &'a Mutex<Store>,
    pub calendar: Option<&'a dyn CalendarSource>,
    pub sink: &'a dyn EventSink,
    pub cancel: &'a AtomicBool,
    /// Confirm every "already on the drive" match by reading both files in full. Slower: reads those
    /// files from the card again. Without it a match is size + first 64 KiB + last 16 KiB, which can't
    /// see damage in the middle of the drive's copy.
    pub full_check: bool,
}

fn cancelled(c: &AtomicBool) -> Result<()> {
    if c.load(Ordering::Relaxed) { Err(CoreError::Other("stopped".into())) } else { Ok(()) }
}

pub fn gather(g: Gather<'_>) -> Result<PlanInput> {
    let library_root = g.lib.root_display();
    let prefix = prefix_for(&library_root);
    let library_id = g.lib.read_text(&format!("{prefix}{LIBRARY_ID_FILE}")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let cache_key = library_id.clone().unwrap_or_else(|| format!("path:{library_root}"));
    let space = g.lib.space()?;

    // 1. The card: every photo and video, read once each for its fingerprint and metadata.
    // Strict: an unreadable folder stops the plan rather than hiding photos from "all backed up".
    let listing = g.card.list_complete()?;
    let mut skipped: BTreeMap<String, u64> = BTreeMap::new();
    let mut media = Vec::new();
    for e in listing {
        match FileKind::of(&e.rel_path) {
            Some(kind) => media.push((e, kind)),
            None => *skipped.entry(extension(&e.rel_path).map(|x| x.to_uppercase()).unwrap_or_else(|| "(none)".into())).or_default() += 1,
        }
    }
    media.sort_by(|a, b| a.0.rel_path.cmp(&b.0.rel_path));
    let total = media.len() as u64;
    let mut files = Vec::with_capacity(media.len());
    let mut makes: Vec<Option<String>> = Vec::with_capacity(media.len());
    let mut unreadable = Vec::new();
    for (i, (e, kind)) in media.into_iter().enumerate() {
        cancelled(g.cancel)?;
        g.sink.emit(CoreEvent::OffloadProgress {
            phase: "card".into(),
            done: i as u64,
            total,
            bytes_done: 0,
            bytes_total: 0,
            current: e.rel_path.clone(),
        });
        let read = (|| -> Result<_> {
            let mut r = g.card.open(&e.rel_path)?;
            let fp = fingerprint(&mut r, e.size)?;
            let meta = read_file_meta(kind, &e.rel_path, r.as_mut());
            Ok((fp, meta))
        })();
        match read {
            Ok((fp, meta)) => {
                files.push(CardFile {
                    rel_path: e.rel_path,
                    size: e.size,
                    kind,
                    fingerprint: fp,
                    captured_at: meta.captured_at.or_else(|| e.modified.map(local_naive)),
                    brand: String::new(),
                    on_drive: None,
                });
                makes.push(meta.make.as_deref().and_then(brand_from_make));
            }
            Err(err) => unreadable.push((e.rel_path, err.to_string())),
        }
    }
    assign_brands(&mut files, &makes);

    // 2. The drive: list everything; fingerprint only files whose size matches a card file.
    let sizes: HashSet<u64> = files.iter().map(|f| f.size).collect();
    let own = format!("{prefix}{OWN_DIR}/").to_lowercase();
    let mut existing_files = HashSet::new();
    let mut existing_dirs = HashSet::new();
    let mut candidates = Vec::new();
    let mut by_name_size: HashMap<(String, u64), Vec<String>> = HashMap::new();
    for e in g.library.list()? {
        let lower = e.rel_path.to_lowercase();
        let mut dir = parent(&lower);
        while !dir.is_empty() && existing_dirs.insert(dir.to_string()) {
            dir = parent(dir);
        }
        if lower.starts_with(&own) {
            existing_files.insert(lower);
            continue;
        }
        by_name_size.entry((file_name(&lower).to_string(), e.size)).or_default().push(e.rel_path.clone());
        existing_files.insert(lower);
        if sizes.contains(&e.size) && FileKind::of(&e.rel_path).is_some() {
            candidates.push(e);
        }
    }
    let cached = g.store.lock().unwrap().offload_cached_files(&cache_key)?;
    let mut fresh = Vec::new();
    let mut on_drive: HashMap<String, Vec<String>> = HashMap::new();
    let ctotal = candidates.len() as u64;
    for (i, e) in candidates.into_iter().enumerate() {
        cancelled(g.cancel)?;
        let mtime = millis(e.modified);
        let fp = match cached.get(&e.rel_path) {
            Some(c) if c.size == e.size && c.mtime == mtime => c.fingerprint.clone(),
            _ => {
                g.sink.emit(CoreEvent::OffloadProgress {
                    phase: "library".into(),
                    done: i as u64,
                    total: ctotal,
                    bytes_done: 0,
                    bytes_total: 0,
                    current: e.rel_path.clone(),
                });
                let Ok(fp) = g.library.open(&e.rel_path).and_then(|mut r| fingerprint(&mut r, e.size)) else { continue };
                fresh.push((e.rel_path.clone(), CachedFile { size: e.size, mtime, fingerprint: fp.clone() }));
                fp
            }
        };
        on_drive.entry(fp).or_default().push(e.rel_path);
    }
    if !fresh.is_empty() {
        g.store.lock().unwrap().offload_cache_files(&cache_key, &fresh)?;
    }
    let matched: Vec<usize> = (0..files.len()).filter(|i| on_drive.contains_key(&files[*i].fingerprint)).collect();
    let check_total = matched.len() as u64;
    for (n, i) in matched.into_iter().enumerate() {
        let f = &mut files[i];
        let paths = &on_drive[&f.fingerprint];
        if !g.full_check {
            f.on_drive = paths.first().cloned();
            continue;
        }
        cancelled(g.cancel)?;
        g.sink.emit(CoreEvent::OffloadProgress {
            phase: "check".into(),
            done: n as u64,
            total: check_total,
            bytes_done: 0,
            bytes_total: 0,
            current: f.rel_path.clone(),
        });
        // The card and the drive are different devices: read both copies at the same time.
        let (first, others) = paths.split_first().expect("matches have paths");
        let (card_hash, drive_hash) = std::thread::scope(|s| {
            let drive = s.spawn(|| full_hash(g.library, first));
            (full_hash(g.card, &f.rel_path), drive.join().unwrap_or_else(|_| Err(CoreError::Other("read failed".into()))))
        });
        // A card file that can't be read stays "not backed up": the copy will try (and report) it.
        let Ok(want) = card_hash else { continue };
        f.on_drive = if drive_hash.is_ok_and(|h| h == want) {
            Some(first.clone())
        } else {
            others.iter().find(|p| full_hash(g.library, p).is_ok_and(|h| h == want)).cloned()
        };
    }

    // 3. Names for folders: saved tags, and all-day / multi-day calendar events over the card's days.
    let days: BTreeSet<NaiveDate> = files.iter().filter_map(CardFile::day).collect();
    let mut spans = Vec::new();
    {
        let store = g.store.lock().unwrap();
        for t in store.list_tags()? {
            let d: BTreeSet<NaiveDate> = t.days.iter().filter_map(|d| d.parse().ok()).collect();
            if !d.is_empty() {
                spans.push(Span { name: t.name, days: d, saved: true });
            }
        }
    }
    if let (Some(cal), Some(first), Some(last)) = (g.calendar, days.first(), days.last()) {
        for ev in cal.events(&first.to_string(), &last.to_string()).unwrap_or_default() {
            let (Ok(a), Ok(b)) = (ev.start_day.parse::<NaiveDate>(), ev.end_day.parse::<NaiveDate>()) else { continue };
            if ev.all_day && a <= b {
                spans.push(Span { name: ev.title, days: a.iter_days().take_while(|d| *d <= b).collect(), saved: false });
            }
        }
    }

    // 4. Folders earlier offloads made on this drive; found again by their files if renamed in Finder.
    let mut remembered = HashMap::new();
    let mut last_backups = Vec::new();
    if let Some(id) = &library_id {
        let store = g.store.lock().unwrap();
        for ((brand, year, key), rel) in store.offload_folders(id)? {
            if existing_dirs.contains(&rel.to_lowercase()) {
                remembered.insert((brand, year, key), rel);
                continue;
            }
            let moved = store.offload_files_in_folder(id, &brand, year, &key)?.into_iter().find_map(|(dest, size)| {
                let hits = by_name_size.get(&(file_name(&dest).to_lowercase(), size))?;
                let p = parent(hits.first()?);
                let last = p.rsplit('/').next().unwrap_or("");
                Some(if ["RAW", "HIF", "Videos"].iter().any(|s| s.eq_ignore_ascii_case(last)) { parent(p).to_string() } else { p.to_string() })
            });
            if let Some(rel) = moved.filter(|r| !r.is_empty()) {
                remembered.insert((brand, year, key), rel);
            }
        }
        last_backups = store.offload_last_backups(id)?;
    }

    Ok(PlanInput {
        card_root: g.card_root,
        library_root,
        prefix,
        library_id,
        cache_key,
        files,
        skipped,
        unreadable,
        spans,
        existing_files,
        existing_dirs,
        remembered,
        space,
        last_backups,
        full_check: g.full_check,
    })
}

/// xxh3-128 of a whole file, read front to back.
fn full_hash(src: &dyn MediaSource, rel: &str) -> Result<u128> {
    let mut r = src.open_streaming(rel)?;
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; 4 << 20];
    loop {
        let n = std::io::Read::read(&mut r, &mut buf)?;
        if n == 0 {
            return Ok(h.digest128());
        }
        h.update(&buf[..n]);
    }
}

/// Brand per file: EXIF Make; else the brand most files in the same card folder have; else the camera's
/// file naming or DCIM folder name; else "Unknown camera".
fn assign_brands(files: &mut [CardFile], makes: &[Option<String>]) {
    let mut per_dir: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for (f, m) in files.iter().zip(makes) {
        if let Some(b) = m {
            *per_dir.entry(parent(&f.rel_path).to_string()).or_default().entry(b.clone()).or_default() += 1;
        }
    }
    for (f, m) in files.iter_mut().zip(makes) {
        f.brand = m
            .clone()
            .or_else(|| per_dir.get(parent(&f.rel_path)).and_then(|c| c.iter().max_by_key(|(b, n)| (**n, std::cmp::Reverse((*b).clone()))).map(|(b, _)| b.clone())))
            .or_else(|| brand_from_file_name(&f.rel_path))
            .or_else(|| brand_from_card_folder(&f.rel_path))
            .unwrap_or_else(|| UNKNOWN_BRAND.into());
    }
}

/// Folder id shown to the UI and used by `FolderChoice`.
fn folder_id(brand: &str, year: i32, key: &FolderKey) -> String {
    format!("{brand}|{year}|{}", key.db_key())
}

/// Where every new file goes, given the user's folder choices. Pure: no reads.
pub fn layout(input: &PlanInput, choices: &[FolderChoice]) -> (Vec<PlannedFile>, OffloadPlan) {
    let choice = |id: &str| choices.iter().find(|c| c.id == id);

    // Group new files by (brand, year, folder key).
    struct Group {
        files: Vec<usize>,
        split_from: BTreeSet<(String, String)>,
    }
    let mut groups: BTreeMap<(String, i32, FolderKey), Group> = BTreeMap::new();
    // Trips are keyed case-insensitively ("Japan trip" tag + "JAPAN TRIP" event = one folder, one id).
    // The folder takes the spelling of a saved tag over a calendar event, then of the longest span.
    let mut trip_names: HashMap<String, String> = HashMap::new();
    let mut ranked: Vec<&Span> = input.spans.iter().collect();
    ranked.sort_by_key(|s| (std::cmp::Reverse(s.saved), std::cmp::Reverse(s.days.len()), s.first(), s.name.clone()));
    for s in ranked {
        trip_names.entry(s.name.to_lowercase()).or_insert_with(|| s.name.clone());
    }
    for (i, f) in input.files.iter().enumerate() {
        if f.on_drive.is_some() {
            continue;
        }
        let (year, key, split) = match f.day() {
            None => (0, FolderKey::Undated, None),
            Some(day) => match broadest(&input.spans, day) {
                Some(span) => {
                    let year = span.first().year();
                    let trip = FolderKey::Trip(span.name.to_lowercase());
                    let id = folder_id(&f.brand, year, &trip);
                    if choice(&id).is_some_and(|c| c.by_month) {
                        (day.year(), FolderKey::month_of(day), Some((id, span.name.clone())))
                    } else {
                        (year, trip, None)
                    }
                }
                None => (day.year(), FolderKey::month_of(day), None),
            },
        };
        let g = groups.entry((f.brand.clone(), year, key)).or_insert_with(|| Group { files: Vec::new(), split_from: BTreeSet::new() });
        g.files.push(i);
        g.split_from.extend(split);
    }

    let mut taken: HashSet<String> = HashSet::new();
    let mut planned = Vec::new();
    let mut folders = Vec::new();
    for ((brand, year, key), g) in groups {
        let id = folder_id(&brand, year, &key);
        let db_key = key.db_key();
        let base = if key == FolderKey::Undated { format!("{}{brand}", input.prefix) } else { format!("{}{brand}/{year}", input.prefix) };
        let typed = choice(&id).and_then(|c| c.name.as_deref()).map(str::trim).filter(|n| !n.is_empty());
        let folder_rel = match typed {
            Some(n) => format!("{base}/{}", sanitize(n)),
            None => input.remembered.get(&(brand.clone(), year, db_key.clone())).cloned().unwrap_or_else(|| {
                let name = match &key {
                    FolderKey::Trip(lower) => sanitize(trip_names.get(lower).unwrap_or(lower)),
                    k => k.default_name(),
                };
                format!("{base}/{name}")
            }),
        };
        let name = file_name(&folder_rel).to_string();

        // Names: RAW+JPEG pairs (same card folder and stem) share one suffix so they still match.
        let mut buckets: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for &i in &g.files {
            let f = &input.files[i];
            let fname = file_name(&f.rel_path);
            let stem = fname.rsplit_once('.').map_or(fname, |(s, _)| s).to_lowercase();
            buckets.entry((parent(&f.rel_path).to_string(), stem)).or_default().push(i);
        }
        for (_, members) in buckets {
            let target = |i: usize, n: u32| {
                let f = &input.files[i];
                let fname = file_name(&f.rel_path);
                let (stem, ext) = fname.rsplit_once('.').unwrap_or((fname, ""));
                let name = if n == 0 { fname.to_string() } else { format!("{stem}-{n}.{ext}") };
                match f.kind.subfolder() {
                    Some(sub) => format!("{folder_rel}/{sub}/{name}"),
                    None => format!("{folder_rel}/{name}"),
                }
            };
            let n = (0..)
                .find(|&n| {
                    let names: Vec<String> = members.iter().map(|&i| target(i, n).to_lowercase()).collect();
                    let distinct: HashSet<&String> = names.iter().collect();
                    distinct.len() == names.len() && names.iter().all(|t| !input.existing_files.contains(t) && !taken.contains(t))
                })
                .expect("some suffix is free");
            for &i in &members {
                let dest = target(i, n);
                taken.insert(dest.to_lowercase());
                planned.push(PlannedFile { index: i, dest_rel: dest, brand: brand.clone(), year, folder_key: db_key.clone(), folder_rel: folder_rel.clone() });
            }
        }

        let files: Vec<&CardFile> = g.files.iter().map(|&i| &input.files[i]).collect();
        let count = |k: FileKind| files.iter().filter(|f| f.kind == k).count() as u64;
        let fdays: BTreeSet<NaiveDate> = files.iter().filter_map(|f| f.day()).collect();
        let mut options: Vec<&Span> = input.spans.iter().filter(|s| s.days.iter().any(|d| fdays.contains(d))).collect();
        options.sort_by(|a, b| b.days.len().cmp(&a.days.len()).then(a.name.cmp(&b.name)));
        let mut seen = HashSet::new();
        let options = options.into_iter().map(|s| sanitize(&s.name)).filter(|n| *n != name && seen.insert(n.to_lowercase())).collect();
        folders.push(PlanFolder {
            id,
            brand: brand.clone(),
            year: (year != 0).then_some(year),
            name,
            exists: input.existing_dirs.contains(&folder_rel.to_lowercase()),
            path: folder_rel,
            is_trip: matches!(key, FolderKey::Trip(_)),
            files: files.len() as u64,
            bytes: files.iter().map(|f| f.size).sum(),
            jpeg: count(FileKind::Jpeg),
            raw: count(FileKind::Raw),
            heif: count(FileKind::Heif),
            video: count(FileKind::Video),
            first_day: fdays.first().map(|d| d.to_string()),
            last_day: fdays.last().map(|d| d.to_string()),
            options,
            split_from: g.split_from.into_iter().collect(),
        });
    }
    // Copy in card order: sequential reads are what SD cards do fastest.
    planned.sort_by(|a, b| input.files[a.index].rel_path.cmp(&input.files[b.index].rel_path));

    let new_bytes: u64 = planned.iter().map(|p| input.files[p.index].size).sum();
    let already: Vec<&CardFile> = input.files.iter().filter(|f| f.on_drive.is_some()).collect();
    let mut warnings = Vec::new();
    if matches!(input.space.fs_type.as_str(), "exfat" | "msdos" | "fat32" | "vfat") {
        warnings.push(format!(
            "This drive is formatted {}. It has no journal, so a power cut or unplugging it mid-copy can damage its folders. APFS is safer for a backup drive.",
            input.space.fs_type.to_uppercase()
        ));
    }
    if input.space.card_like {
        warnings.push("The backup drive has a DCIM folder at its top, like a camera card. Make sure it's your backup drive and not a card.".into());
    }
    if new_bytes > 0 && new_bytes + SPACE_MARGIN > input.space.free_bytes {
        warnings.push("Not enough free space on this drive for the new files (plus 1 GB to spare).".into());
    }
    // FAT32 can't hold a file of 4 GiB or more: those copies would fail.
    const FAT32_MAX: u64 = (4 << 30) - 1;
    let too_big = planned.iter().filter(|p| input.files[p.index].size > FAT32_MAX).count();
    if too_big > 0 && matches!(input.space.fs_type.as_str(), "msdos" | "fat32" | "vfat") {
        warnings.push(format!(
            "{too_big} {} 4 GB or larger, which this drive's format (FAT32) can't store. Use an exFAT or APFS drive for videos.",
            if too_big == 1 { "file is" } else { "files are" }
        ));
    }
    let plan = OffloadPlan {
        card_root: input.card_root.clone(),
        library_root: input.library_root.clone(),
        new_files: planned.len() as u64,
        new_bytes,
        already_files: already.len() as u64,
        already_bytes: already.iter().map(|f| f.size).sum(),
        skipped: input.skipped.iter().map(|(e, n)| CountByType { extension: e.clone(), count: *n }).collect(),
        unreadable: input.unreadable.iter().map(|(p, e)| FailedFile { path: p.clone(), error: e.clone() }).collect(),
        folders,
        free_bytes: input.space.free_bytes,
        fs_type: input.space.fs_type.clone(),
        warnings,
        last_backups: input.last_backups.clone(),
        full_check: input.full_check,
    };
    (planned, plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Option<NaiveDateTime> {
        Some(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap())
    }
    fn file(path: &str, when: &str, brand: &str) -> CardFile {
        CardFile {
            rel_path: path.into(),
            size: 100,
            kind: FileKind::of(path).unwrap(),
            fingerprint: format!("fp-{path}"),
            captured_at: t(when),
            brand: brand.into(),
            on_drive: None,
        }
    }
    fn days(a: &str, b: &str) -> BTreeSet<NaiveDate> {
        let (a, b): (NaiveDate, NaiveDate) = (a.parse().unwrap(), b.parse().unwrap());
        a.iter_days().take_while(|d| *d <= b).collect()
    }
    fn input(files: Vec<CardFile>, spans: Vec<Span>) -> PlanInput {
        PlanInput {
            card_root: "/Volumes/SD".into(),
            library_root: "/Volumes/T7".into(),
            prefix: "Photos/".into(),
            library_id: None,
            cache_key: "k".into(),
            files,
            skipped: BTreeMap::new(),
            unreadable: vec![],
            spans,
            existing_files: HashSet::new(),
            existing_dirs: HashSet::new(),
            remembered: HashMap::new(),
            space: LibrarySpace { free_bytes: u64::MAX / 2, fs_type: "apfs".into(), card_like: false },
            last_backups: vec![],
            full_check: false,
        }
    }
    fn dests(planned: &[PlannedFile]) -> Vec<&str> {
        planned.iter().map(|p| p.dest_rel.as_str()).collect()
    }

    #[test]
    fn trips_months_brands_and_kinds() {
        let japan = Span { name: "Japan trip".into(), days: days("2026-10-01", "2026-10-14"), saved: true };
        let kyoto = Span { name: "Kyoto".into(), days: days("2026-10-05", "2026-10-06"), saved: true };
        let files = vec![
            file("DCIM/100_FUJI/DSCF0001.JPG", "2026-10-05T10:00:00", "Fuji"),
            file("DCIM/100_FUJI/DSCF0001.RAF", "2026-10-05T10:00:00", "Fuji"),
            file("DCIM/100_FUJI/DSCF0002.MOV", "2026-10-15T03:00:00", "Fuji"), // 3 a.m. → still Oct 14: Japan
            file("DCIM/100_FUJI/DSCF0003.HIF", "2026-11-02T12:00:00", "Fuji"),
            file("DCIM/100NZ6_2/DSC_0001.NEF", "2026-10-02T12:00:00", "Nikon"),
        ];
        let (planned, plan) = layout(&input(files, vec![japan, kyoto]), &[]);
        assert_eq!(
            dests(&planned),
            [
                // Card order ("100NZ6_2" sorts before "100_FUJI").
                "Photos/Nikon/2026/Japan trip/RAW/DSC_0001.NEF",
                "Photos/Fuji/2026/Japan trip/DSCF0001.JPG",
                "Photos/Fuji/2026/Japan trip/RAW/DSCF0001.RAF",
                "Photos/Fuji/2026/Japan trip/Videos/DSCF0002.MOV",
                "Photos/Fuji/2026/2026-11/HIF/DSCF0003.HIF",
            ]
        );
        let trip = plan.folders.iter().find(|f| f.id == "Fuji|2026|trip:japan trip").unwrap();
        assert_eq!((trip.files, trip.jpeg, trip.raw, trip.video), (3, 1, 1, 1));
        assert_eq!(trip.options, ["Kyoto"]);
        assert!(trip.is_trip);
    }

    #[test]
    fn choices_rename_or_split_by_month() {
        let japan = Span { name: "Japan trip".into(), days: days("2026-09-28", "2026-10-03"), saved: false };
        let files = vec![file("DCIM/A/DSCF0001.JPG", "2026-09-29T10:00:00", "Fuji"), file("DCIM/A/DSCF0002.JPG", "2026-10-02T10:00:00", "Fuji")];
        let inp = input(files, vec![japan]);
        let id = "Fuji|2026|trip:japan trip".to_string();
        let (p, _) = layout(&inp, &[FolderChoice { id: id.clone(), name: Some("Japan: autumn".into()), by_month: false }]);
        assert_eq!(dests(&p), ["Photos/Fuji/2026/Japan- autumn/DSCF0001.JPG", "Photos/Fuji/2026/Japan- autumn/DSCF0002.JPG"]);
        let (p, plan) = layout(&inp, &[FolderChoice { id: id.clone(), name: None, by_month: true }]);
        assert_eq!(dests(&p), ["Photos/Fuji/2026/2026-09/DSCF0001.JPG", "Photos/Fuji/2026/2026-10/DSCF0002.JPG"]);
        assert!(plan.folders.iter().all(|f| f.split_from == [(id.clone(), "Japan trip".to_string())]));
    }

    #[test]
    fn names_never_clash_and_pairs_keep_one_suffix() {
        let mut inp = input(
            vec![
                file("DCIM/100_FUJI/DSCF0001.JPG", "2026-11-02T12:00:00", "Fuji"),
                file("DCIM/100_FUJI/DSCF0001.RAF", "2026-11-02T12:00:00", "Fuji"),
                file("DCIM/101_FUJI/DSCF0001.JPG", "2026-11-03T12:00:00", "Fuji"),
            ],
            vec![],
        );
        inp.existing_files.insert("photos/fuji/2026/2026-11/raw/dscf0001.raf".into()); // a different photo
        let (p, _) = layout(&inp, &[]);
        assert_eq!(
            dests(&p),
            ["Photos/Fuji/2026/2026-11/DSCF0001-1.JPG", "Photos/Fuji/2026/2026-11/RAW/DSCF0001-1.RAF", "Photos/Fuji/2026/2026-11/DSCF0001.JPG"]
        );
    }

    #[test]
    fn already_on_the_drive_and_remembered_folders() {
        let mut inp = input(
            vec![file("DCIM/A/DSCF0001.JPG", "2026-10-05T10:00:00", "Fuji"), file("DCIM/A/DSCF0002.JPG", "2026-10-05T11:00:00", "Fuji")],
            vec![Span { name: "Japan trip".into(), days: days("2026-10-01", "2026-10-14"), saved: true }],
        );
        inp.files[0].on_drive = Some("Old stuff/whatever.jpg".into());
        inp.remembered.insert(("Fuji".into(), 2026, "trip:japan trip".into()), "Photos/Fuji/2026/Japan 🇯🇵".into());
        let (p, plan) = layout(&inp, &[]);
        assert_eq!(dests(&p), ["Photos/Fuji/2026/Japan 🇯🇵/DSCF0002.JPG"]);
        assert_eq!((plan.new_files, plan.already_files), (1, 1));
    }

    #[test]
    fn undated_and_unknown() {
        let mut f = file("DCIM/A/CLIP0001.MOV", "2026-10-05T10:00:00", UNKNOWN_BRAND);
        f.captured_at = None;
        let (p, _) = layout(&input(vec![f], vec![]), &[]);
        assert_eq!(dests(&p), ["Photos/Unknown camera/Undated/Videos/CLIP0001.MOV"]);
    }

    #[test]
    fn brands_fall_back_by_folder_then_name() {
        let mut files = vec![
            file("DCIM/100_FUJI/DSCF0001.JPG", "2026-10-05T10:00:00", ""),
            file("DCIM/100_FUJI/DSCF0002.MOV", "2026-10-05T10:00:00", ""),
            file("DCIM/999XYZ/DSC_0001.MOV", "2026-10-05T10:00:00", ""),
            file("PRIVATE/CLIP.MP4", "2026-10-05T10:00:00", ""),
        ];
        assign_brands(&mut files, &[Some("Fuji".into()), None, None, None]);
        let b: Vec<&str> = files.iter().map(|f| f.brand.as_str()).collect();
        assert_eq!(b, ["Fuji", "Fuji", "Nikon", UNKNOWN_BRAND]);
    }

    #[test]
    fn warns_when_a_fat32_drive_cant_hold_a_big_video() {
        let mut big = file("DCIM/A/DSCF0001.MOV", "2026-10-05T10:00:00", "Fuji");
        big.size = 5 << 30;
        let mut inp = input(vec![big, file("DCIM/A/DSCF0002.JPG", "2026-10-05T10:00:00", "Fuji")], vec![]);
        inp.space.fs_type = "msdos".into();
        let (_, plan) = layout(&inp, &[]);
        assert!(plan.warnings.iter().any(|w| w.contains("1 file is 4 GB")), "{:?}", plan.warnings);
        inp.space.fs_type = "exfat".into();
        let (_, plan) = layout(&inp, &[]);
        assert!(!plan.warnings.iter().any(|w| w.contains("4 GB")));
    }

    #[test]
    fn prefix() {
        assert_eq!(prefix_for("/Volumes/T7"), "Photos/");
        assert_eq!(prefix_for("/Volumes/T7/Photos"), "");
        assert_eq!(prefix_for("/Volumes/T7/photos/"), "");
    }
}
