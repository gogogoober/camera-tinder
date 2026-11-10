//! Desktop platform: plain-filesystem `MediaSource` and copy-to-folder export.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use cull_core::{CoreError, CoreEvent, EventSink, ExportItem, ExportReport, ExportTarget, MediaEntry, MediaSource, Photo, ReadSeek, Result};
use walkdir::WalkDir;
use xxhash_rust::xxh3::Xxh3;

/// A folder on disk (or a mounted SD card), scanned recursively.
pub struct FsSource {
    root: PathBuf,
    id: String,
    name: String,
    /// Folders never listed (the app's own data and preview cache, if they're inside `root`).
    excluded: Vec<PathBuf>,
}

impl FsSource {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref())?;
        if !root.is_dir() {
            return Err(CoreError::Other(format!("not a folder: {}", root.display())));
        }
        let id = root.to_string_lossy().into_owned();
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone());
        Ok(Self { root, id, name, excluded: Vec::new() })
    }

    /// Never list anything in these folders. Pass the app's own folders
    /// (`CullerService::app_dirs`), so a picked folder that contains them (a home folder, a
    /// whole disk) doesn't index the app's previews as photos.
    pub fn excluding<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.excluded.extend(dirs.into_iter().map(|d| fs::canonicalize(d.as_ref()).unwrap_or_else(|_| d.as_ref().to_path_buf())));
        self
    }

    /// The canonical folder this source reads from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(CoreError::Other(format!("invalid relative path: {rel_path}")));
        }
        Ok(self.root.join(rel))
    }
}

/// Whether this source is reachable *and* lives on a volume other than the Mac's own
/// drive: an SD card, USB/Thunderbolt drive, disk image or network share.
///
/// "Own drive" = the filesystem device of `/`, the macOS Data volume, and the home folder
/// (on APFS these can differ). Anything else counts as external — when unsure we say
/// external, which errs towards keeping the "Clear previews" button locked.
pub fn is_external_and_connected(source_id: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(source_id) else { return false };
    if !meta.is_dir() {
        return false;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let internal: Vec<u64> = [Some(PathBuf::from("/")), Some(PathBuf::from("/System/Volumes/Data")), home]
        .into_iter()
        .flatten()
        .filter_map(|p| fs::metadata(p).ok().map(|m| m.dev()))
        .collect();
    !internal.contains(&meta.dev())
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

impl MediaSource for FsSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }

    fn list(&self) -> Result<Vec<MediaEntry>> {
        let mut out = Vec::new();
        let walker = WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || (!is_hidden(e.file_name()) && !self.excluded.iter().any(|x| e.path().starts_with(x))));
        for entry in walker {
            let Ok(entry) = entry else { continue }; // unreadable dirs: skip, don't abort the scan
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(rel) = entry.path().strip_prefix(&self.root) else { continue };
            let rel_path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            out.push(MediaEntry { rel_path, size: meta.len(), modified: meta.modified().ok() });
        }
        Ok(out)
    }

    fn open(&self, rel_path: &str) -> Result<Box<dyn ReadSeek>> {
        // Read-only, always. This is the only way the app touches a source file.
        let file = OpenOptions::new().read(true).open(self.resolve(rel_path)?)?;
        Ok(Box::new(file))
    }
}

/// v1 export: copy keepers into a folder. Never moves, renames or deletes originals.
///
/// Safety rules (each one has a test in `tests/source_safety.rs`):
/// - Refuses a destination inside any protected folder (the sources being culled), and any
///   destination on the same card or external drive as a connected source, so nothing is ever
///   written onto a card (even outside the folder that was added, e.g. `SD/Keepers` next to `SD/DCIM`).
/// - Never overwrites an existing file: data goes to a uniquely named temp file
///   (`create_new`) and is only linked into place if the final name is still free.
/// - Verifies every copy before it counts: a full-content hash of the bytes read from the
///   source must match the file on disk, and its fingerprint must match the photo that was
///   culled. Otherwise the copy is removed and reported as failed.
/// - Re-running is safe: a file that already holds the same photo is skipped; a
///   different file with the same name gets a `-1`, `-2`… suffix.
pub struct CopyToDir {
    dest: PathBuf,
    protected: Vec<PathBuf>,
}

impl CopyToDir {
    pub fn new(dest: impl Into<PathBuf>) -> Self {
        Self { dest: dest.into(), protected: Vec::new() }
    }

    /// Folders that must never receive files (typically every source root).
    pub fn protecting<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.protected.extend(roots.into_iter().map(|p| p.as_ref().to_path_buf()));
        self
    }

    /// Resolve `p` as far as it exists (it may not exist yet), so symlinks and `..`
    /// can't hide that the destination is really inside a protected folder.
    fn resolve_existing_prefix(p: &Path) -> io::Result<PathBuf> {
        let abs = p.to_path_buf();
        let mut rest = Vec::new();
        let mut cur = abs.as_path();
        loop {
            match fs::canonicalize(cur) {
                Ok(c) => {
                    let mut out = c;
                    for part in rest.iter().rev() {
                        out.push(part);
                    }
                    return Ok(out);
                }
                Err(_) => {
                    let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) else { return Ok(abs) };
                    rest.push(name.to_os_string());
                    cur = parent;
                }
            }
        }
    }

    fn check_destination(&self) -> Result<PathBuf> {
        if !self.dest.is_absolute() {
            return Err(CoreError::Other(format!("export folder must be a full path: '{}'", self.dest.display())));
        }
        if self.dest.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(CoreError::Other(format!("export folder must not contain '..': {}", self.dest.display())));
        }
        let dest = Self::resolve_existing_prefix(&self.dest)?;
        for root in &self.protected {
            let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            if dest.starts_with(&root) {
                return Err(CoreError::Other(format!(
                    "refusing to export into {}: it is inside a photo source ({}). Pick a folder outside your cards/source folders.",
                    dest.display(),
                    root.display()
                )));
            }
        }
        // Not on a card / external drive that holds a source either. (The Mac's own drive is fine.)
        use std::os::unix::fs::MetadataExt;
        let dest_dev = dest.ancestors().find_map(|a| fs::metadata(a).ok()).map(|m| m.dev());
        for root in &self.protected {
            let root_id = root.to_string_lossy();
            if is_external_and_connected(&root_id) && fs::metadata(root).ok().map(|m| m.dev()) == dest_dev {
                return Err(CoreError::Other(format!(
                    "refusing to export to {}: it is on the same card or drive as a photo source ({}). Pick a folder on another drive.",
                    dest.display(),
                    root.display()
                )));
            }
        }
        Ok(dest)
    }

    /// Where `photo` should go, or `None` if a file there already holds this photo.
    fn target_for(dest: &Path, photo: &Photo) -> Result<Option<PathBuf>> {
        let path = Path::new(&photo.file_name);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        for n in 0..10_000 {
            let candidate = if n == 0 { dest.join(&photo.file_name) } else { dest.join(format!("{stem}-{n}{ext}")) };
            match fs::symlink_metadata(&candidate) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Some(candidate)),
                Err(e) => return Err(e.into()),
                Ok(m) if m.is_file() && m.len() == photo.size && fingerprint_file(&candidate, m.len())? == photo.fingerprint => {
                    return Ok(None); // already exported
                }
                Ok(_) => continue,
            }
        }
        Err(CoreError::Other(format!("too many files named like {}", photo.file_name)))
    }

    fn copy_one(dest: &Path, item: &ExportItem<'_>, n: usize) -> Result<bool> {
        let p = item.photo;
        let Some(target) = Self::target_for(dest, p)? else { return Ok(false) };

        // Unique temp name, created exclusively: never truncates anything that exists.
        let tmp = dest.join(format!(".{}.{}-{n}.partial", p.file_name, std::process::id()));
        let mut out = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let copied = (|| -> Result<()> {
            let mut src = HashingReader { inner: item.source.open(&p.rel_path)?, hash: Xxh3::new() }; // read-only
            let written = io::copy(&mut src, &mut out)?;
            out.sync_all()?;
            drop(out);
            // Full-content check: what landed on disk is bit-for-bit what we read from the card.
            let mut written_hash = Xxh3::new();
            io::copy(&mut File::open(&tmp)?, &mut HashWriter(&mut written_hash))?;
            if written_hash.digest128() != src.hash.digest128() {
                return Err(CoreError::Other("copy is corrupt (bytes on disk differ from the original); not exported".into()));
            }
            if written != p.size {
                return Err(CoreError::Other(format!("source changed since scan: {} bytes, expected {}", written, p.size)));
            }
            if fingerprint_file(&tmp, written)? != p.fingerprint {
                return Err(CoreError::Other("copy does not match the photo you culled (file changed since scan?)".into()));
            }
            // Link into place: fails if `target` appeared meanwhile, so nothing is overwritten.
            match fs::hard_link(&tmp, &target) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e.into()),
                // Filesystems without hard links (exFAT/FAT drives; they don't support a no-overwrite
                // rename either): claim the name with a new empty file (fails if anything is there),
                // then move the verified copy onto our own claim.
                Err(_) => {
                    OpenOptions::new().write(true).create_new(true).open(&target)?;
                    if let Err(e) = fs::rename(&tmp, &target) {
                        if fs::symlink_metadata(&target).is_ok_and(|m| m.is_file() && m.len() == 0) {
                            let _ = fs::remove_file(&target); // our own empty claim, nothing else
                        }
                        return Err(e.into());
                    }
                    Ok(())
                }
            }
        })();
        // Only ever removes our own temp file.
        let _ = fs::remove_file(&tmp);
        copied.map(|()| true)
    }
}

struct HashingReader<R> {
    inner: R,
    hash: Xxh3,
}
impl<R: io::Read> io::Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// An in-memory hash sink (not a file).
struct HashWriter<'a>(&'a mut Xxh3);
impl io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fingerprint_file(path: &Path, size: u64) -> Result<String> {
    cull_core::identity::fingerprint(&mut File::open(path)?, size)
}

impl ExportTarget for CopyToDir {
    fn name(&self) -> &str {
        "Copy to folder"
    }

    fn export(&self, items: &[ExportItem<'_>], sink: &dyn EventSink) -> Result<ExportReport> {
        let dest = self.check_destination()?;
        fs::create_dir_all(&dest)?;
        let mut report = ExportReport { destination: dest.to_string_lossy().into_owned(), ..Default::default() };
        let total = items.len() as u64;
        for (i, item) in items.iter().enumerate() {
            match Self::copy_one(&dest, item, i) {
                Ok(true) => report.copied += 1,
                Ok(false) => report.already_there += 1,
                Err(e) => report.failed.push((item.photo.rel_path.clone(), e.to_string())),
            }
            sink.emit(CoreEvent::ExportProgress { done: i as u64 + 1, total });
        }
        Ok(report)
    }
}
