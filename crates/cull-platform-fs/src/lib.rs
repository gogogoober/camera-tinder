//! Desktop platform: plain-filesystem `MediaSource` and copy-to-folder export.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use cull_core::{CoreError, CoreEvent, EventSink, ExportItem, ExportReport, ExportTarget, MediaEntry, MediaSource, ReadSeek, Result};
use walkdir::WalkDir;

/// A folder on disk (or a mounted SD card), scanned recursively.
pub struct FsSource {
    root: PathBuf,
    id: String,
    name: String,
}

impl FsSource {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref())?;
        if !root.is_dir() {
            return Err(CoreError::Other(format!("not a folder: {}", root.display())));
        }
        let id = root.to_string_lossy().into_owned();
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone());
        Ok(Self { root, id, name })
    }

    fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(CoreError::Other(format!("invalid relative path: {rel_path}")));
        }
        Ok(self.root.join(rel))
    }
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
        let walker = WalkDir::new(&self.root).follow_links(false).into_iter().filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()));
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
        Ok(Box::new(File::open(self.resolve(rel_path)?)?))
    }
}

/// v1 export: copy keepers into a folder. Never moves or deletes originals.
/// Re-running is safe: files already copied (same name and size) are skipped;
/// different files with clashing names get a `-1`, `-2`… suffix.
pub struct CopyToDir {
    dest: PathBuf,
}

impl CopyToDir {
    pub fn new(dest: impl Into<PathBuf>) -> Self {
        Self { dest: dest.into() }
    }

    fn target_for(&self, file_name: &str, size: u64) -> io::Result<Option<PathBuf>> {
        let path = Path::new(file_name);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        for n in 0.. {
            let candidate = if n == 0 { self.dest.join(file_name) } else { self.dest.join(format!("{stem}-{n}{ext}")) };
            match fs::metadata(&candidate) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Some(candidate)),
                Err(e) => return Err(e),
                Ok(m) if m.len() == size => return Ok(None), // already exported
                Ok(_) => continue,
            }
        }
        unreachable!()
    }
}

impl ExportTarget for CopyToDir {
    fn name(&self) -> &str {
        "Copy to folder"
    }

    fn export(&self, items: &[ExportItem<'_>], sink: &dyn EventSink) -> Result<ExportReport> {
        fs::create_dir_all(&self.dest)?;
        let mut report = ExportReport { destination: self.dest.to_string_lossy().into_owned(), ..Default::default() };
        let total = items.len() as u64;
        for (i, item) in items.iter().enumerate() {
            let p = item.photo;
            let result: Result<bool> = (|| {
                let Some(target) = self.target_for(&p.file_name, p.size)? else { return Ok(false) };
                let mut src = item.source.open(&p.rel_path)?;
                let tmp = target.with_file_name(format!(".{}.partial", p.file_name));
                let mut out = File::create(&tmp)?;
                io::copy(&mut src, &mut out)?;
                out.sync_all()?;
                fs::rename(&tmp, &target)?;
                Ok(true)
            })();
            match result {
                Ok(true) => report.copied += 1,
                Ok(false) => report.already_there += 1,
                Err(e) => report.failed.push((p.rel_path.clone(), e.to_string())),
            }
            sink.emit(CoreEvent::ExportProgress { done: i as u64 + 1, total });
        }
        Ok(report)
    }
}
