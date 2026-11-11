//! Offload destination on a plain filesystem: the backup drive. See docs/offload.md.
//!
//! Safety rules (tests in `tests/offload.rs` and `tests/source_safety.rs`):
//! - Only adds files under the picked folder. Never overwrites: temp files are created new, and the final
//!   name is taken with a no-overwrite link (or, on exFAT/FAT, an exclusive empty claim + rename onto it).
//! - The only files it removes are its own temp files (`.<name>.darkroom-<tag>.partial`) and the empty
//!   claim it just made.
//! - Writes and read-backs bypass the OS cache (`F_NOCACHE`), so a read-back really reads the drive.
//!   `flush_to_media` asks the drive to write its own cache to the medium (`F_FULLFSYNC`).
//! - Refuses a folder on the same volume as the card, so nothing is ever written onto a card, and a drive
//!   that looks like a camera card (DCIM at its top) when the card doesn't (the two picks swapped).
//! - Every folder it writes in must resolve inside the picked folder, on the picked folder's volume: a
//!   symlink on the drive can't redirect writes elsewhere (onto the card, another drive, a system folder).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use cull_core::{CoreError, Library, LibrarySpace, PartialWrite, Result};
use xxhash_rust::xxh3::Xxh3;

const CHUNK: usize = 4 << 20;

pub struct FsLibrary {
    root: PathBuf,
    /// The picked folder's volume: nothing is written on any other.
    dev: u64,
    /// The drive has a DCIM folder at its top, like a camera card.
    card_like: bool,
}

fn device(p: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(p)?.dev())
}

/// Looks like a camera card: the folder, or a folder above it on the same volume, has a DCIM folder (or is
/// one). Picking `/Volumes/SD`, `/Volumes/SD/DCIM` or `/Volumes/SD/DCIM/100NIKON` all count.
fn looks_like_card(p: &Path) -> bool {
    let Ok(dev) = device(p) else { return false };
    p.ancestors().take_while(|a| device(a).ok() == Some(dev)).any(|a| a.file_name().is_some_and(|n| n == "DCIM") || a.join("DCIM").is_dir())
}

/// Shared checks of both constructors: not nested, and not the two picks swapped.
fn check_pair(root: &Path, card: &Path) -> Result<bool> {
    if !root.is_dir() {
        return Err(CoreError::Other(format!("not a folder: {}", root.display())));
    }
    if root.starts_with(card) || card.starts_with(root) {
        return Err(CoreError::Other("the backup folder and the card can't contain each other".into()));
    }
    let card_like = looks_like_card(root);
    if card_like && !looks_like_card(card) {
        return Err(CoreError::Other(
            "the backup drive looks like a camera card (it has a DCIM folder) and the card doesn't: were they picked the other way round?".into(),
        ));
    }
    Ok(card_like)
}

impl FsLibrary {
    /// The backup drive's folder. Refuses one on the card's volume, inside the card folder, or containing it.
    pub fn new(root: impl AsRef<Path>, card_root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref())?;
        let card = fs::canonicalize(card_root.as_ref())?;
        let card_like = check_pair(&root, &card)?;
        let dev = device(&root)?;
        if dev == device(&card)? {
            return Err(CoreError::Other("pick a backup drive other than the card: they are on the same volume".into()));
        }
        Ok(Self { root, dev, card_like })
    }

    /// Tests only: like `new`, but allows the card and the drive on one volume (temp folders). Still
    /// refuses one folder inside the other. The app never calls this (checked in `tests/source_safety.rs`).
    #[doc(hidden)]
    pub fn allowing_same_volume_for_tests(root: impl AsRef<Path>, card_root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref())?;
        let card = fs::canonicalize(card_root.as_ref())?;
        let card_like = check_pair(&root, &card)?;
        Ok(Self { dev: device(&root)?, root, card_like })
    }

    /// A folder (relative) to write in, created one level at a time without following any symlink that
    /// leads out of the picked folder or onto another volume. Returns its real path.
    fn ensure_dir(&self, rel_dir: &Path) -> Result<PathBuf> {
        let mut cur = self.root.clone();
        for c in rel_dir.components() {
            let Component::Normal(name) = c else {
                return Err(CoreError::Other(format!("invalid folder: {}", rel_dir.display())));
            };
            cur.push(name);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() => {}
                Ok(m) if m.file_type().is_symlink() => cur = self.inside(&cur)?,
                Ok(_) => return Err(CoreError::Other(format!("{} is a file, not a folder", cur.display()))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&cur) {
                    // offload: one new folder inside the picked backup folder (checked level by level)
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => cur = self.inside(&cur)?,
                    Err(e) => return Err(e.into()),
                },
                Err(e) => return Err(e.into()),
            }
        }
        self.inside(&cur)
    }

    /// `dir`'s real path, if it is a folder inside the picked folder and on its volume.
    fn inside(&self, dir: &Path) -> Result<PathBuf> {
        let real = fs::canonicalize(dir)?;
        if !real.starts_with(&self.root) || device(&real)? != self.dev || !real.is_dir() {
            return Err(CoreError::Other(format!("{} leads outside the backup folder; not writing there", dir.display())));
        }
        Ok(real)
    }

    /// The real folder of a file path under the root, checked like `ensure_dir` but never created.
    fn existing_dir_of(&self, rel_path: &str) -> Result<(PathBuf, std::ffi::OsString)> {
        let path = self.resolve(rel_path)?;
        let name = path.file_name().ok_or_else(|| CoreError::Other(format!("no file name in {rel_path}")))?.to_os_string();
        let dir = self.inside(path.parent().unwrap_or(&self.root))?;
        Ok((dir, name))
    }

    fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        if rel_path.is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(CoreError::Other(format!("invalid relative path: {rel_path}")));
        }
        Ok(self.root.join(rel))
    }
}

/// Our temp file names only: `.<name>.darkroom-<tag>.partial`.
fn is_our_partial(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".partial") && name.contains(".darkroom-")
}

/// Skip the OS cache for this file (macOS). Writes go to the drive without lingering in memory; reads
/// come from the drive, not from pages left by the write.
fn no_cache(f: &File) {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: plain fcntl on an fd we own.
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) };
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = f;
}

/// Hand the file's data to the drive (`fsync`; on macOS this does not flush the drive's own cache).
fn fsync(f: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: plain fsync on an fd we own.
    if unsafe { libc::fsync(f.as_raw_fd()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Make the drive write its own cache to the medium. Falls back to fsync where unsupported.
fn full_fsync(f: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: plain fcntl on an fd we own.
        if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    fsync(f)
}

/// Open a file for reading only, bypassing the OS cache.
pub(crate) fn open_uncached(path: &Path) -> io::Result<File> {
    let f = OpenOptions::new().read(true).open(path)?;
    no_cache(&f);
    Ok(f)
}

struct FsPartial {
    file: File,
    rel: String,
}

impl PartialWrite for FsPartial {
    fn rel_path(&self) -> &str {
        &self.rel
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        Ok(self.file.write_all(buf)?)
    }

    fn finish(self: Box<Self>) -> Result<()> {
        fsync(&self.file)?;
        Ok(())
    }
}

impl Library for FsLibrary {
    fn root_display(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn space(&self) -> Result<LibrarySpace> {
        Ok(LibrarySpace { card_like: self.card_like, ..statfs(&self.root)? })
    }

    fn dir_exists(&self, rel_path: &str) -> bool {
        self.resolve(rel_path).is_ok_and(|p| self.inside(&p).is_ok())
    }

    fn read_text(&self, rel_path: &str) -> Option<String> {
        let mut s = String::new();
        File::open(self.resolve(rel_path).ok()?).ok()?.take(4096).read_to_string(&mut s).ok()?;
        Some(s)
    }

    fn create_text(&self, rel_path: &str, text: &str) -> Result<()> {
        let rel = Path::new(rel_path);
        let path = self.resolve(rel_path)?;
        let dir = self.ensure_dir(rel.parent().unwrap_or(Path::new("")))?;
        let path = dir.join(path.file_name().ok_or_else(|| CoreError::Other(format!("no file name in {rel_path}")))?);
        let mut f = OpenOptions::new().write(true).create_new(true).open(&path)?; // offload: new file, never an existing one
        f.write_all(text.as_bytes())?;
        full_fsync(&f)?;
        Ok(())
    }

    fn begin(&self, dest_rel: &str, tag: &str) -> Result<Box<dyn PartialWrite>> {
        let dest = self.resolve(dest_rel)?;
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tag: String = tag.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        let dir = self.ensure_dir(Path::new(dest_rel).parent().unwrap_or(Path::new("")))?;
        let partial_name = format!(".{name}.darkroom-{tag}.partial");
        let file = OpenOptions::new().write(true).create_new(true).open(dir.join(&partial_name))?; // offload: new temp file
        no_cache(&file);
        let rel = match dest_rel.rsplit_once('/') {
            Some((d, _)) => format!("{d}/{partial_name}"),
            None => partial_name,
        };
        Ok(Box::new(FsPartial { file, rel }))
    }

    fn flush_to_media(&self, rel_path: &str) -> Result<()> {
        let f = File::open(self.resolve(rel_path)?)?;
        full_fsync(&f)?;
        Ok(())
    }

    fn verify(&self, rel_path: &str, size: u64, hash: u128) -> Result<()> {
        let mut f = open_uncached(&self.resolve(rel_path)?)?;
        let mut h = Xxh3::new();
        let mut buf = vec![0u8; CHUNK];
        let mut len = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            len += n as u64;
        }
        if len != size {
            return Err(CoreError::Other(format!("{len} bytes on the drive, expected {size}")));
        }
        if h.digest128() != hash {
            return Err(CoreError::Other("the bytes on the drive differ from the card".into()));
        }
        Ok(())
    }

    fn commit(&self, partial_rel: &str, dest_rel: &str) -> Result<Option<SystemTime>> {
        let (pdir, pname) = self.existing_dir_of(partial_rel)?;
        let (ddir, dname) = self.existing_dir_of(dest_rel)?;
        let (partial, dest) = (pdir.join(&pname), ddir.join(&dname));
        if !is_our_partial(&pname.to_string_lossy()) {
            return Err(CoreError::Other(format!("not an offload temp file: {partial_rel}")));
        }
        match fs::hard_link(&partial, &dest) {
            // offload: fails if `dest` exists, so nothing is overwritten
            Ok(()) => {
                fs::remove_file(&partial)?; // offload: our own temp file (the photo now has its final name)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(CoreError::Other(format!("{dest_rel} appeared on the drive meanwhile; plan again")));
            }
            // exFAT/FAT: no hard links, no no-overwrite rename. Claim the name with a new empty file
            // (fails if anything is there), then move the verified copy onto our own claim.
            Err(_) => {
                OpenOptions::new().write(true).create_new(true).open(&dest)?; // offload: exclusive claim of the final name
                if let Err(e) = fs::rename(&partial, &dest) {
                    // offload: onto our own empty claim
                    if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_file() && m.len() == 0) {
                        let _ = fs::remove_file(&dest); // offload: our own empty claim, nothing else
                    }
                    return Err(e.into());
                }
            }
        }
        if let Some(dir) = dest.parent() {
            let _ = File::open(dir).and_then(|d| fsync(&d));
        }
        Ok(fs::metadata(&dest)?.modified().ok())
    }

    fn discard(&self, partial_rel: &str) -> Result<()> {
        if !Path::new(partial_rel).file_name().is_some_and(|n| is_our_partial(&n.to_string_lossy())) {
            return Err(CoreError::Other(format!("not an offload temp file: {partial_rel}")));
        }
        let (dir, name) = match self.existing_dir_of(partial_rel) {
            Ok(x) => x,
            // Its folder is gone: so is the temp file.
            Err(CoreError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(m) if !m.is_file() => Err(CoreError::Other(format!("not a file: {partial_rel}"))),
            Ok(_) => Ok(fs::remove_file(&path)?), // offload: our own temp file only (name checked above)
        }
    }
}

#[cfg(target_vendor = "apple")]
fn statfs(path: &Path) -> Result<LibrarySpace> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| CoreError::Other(e.to_string()))?;
    // SAFETY: zeroed is a valid statfs; statfs fills it for a valid C path.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: f_fstypename is NUL-terminated by the kernel.
    let fs_type = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy().to_lowercase();
    Ok(LibrarySpace { free_bytes: st.f_bavail * st.f_bsize as u64, fs_type, card_like: false })
}

#[cfg(not(target_vendor = "apple"))]
fn statfs(path: &Path) -> Result<LibrarySpace> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| CoreError::Other(e.to_string()))?;
    // SAFETY: zeroed is a valid statvfs; statvfs fills it for a valid C path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(LibrarySpace { free_bytes: st.f_bavail as u64 * st.f_frsize as u64, fs_type: "unknown".into(), card_like: false })
}
