//! Content-based photo identity.
//!
//! A photo's path changes when a card is copied to disk, so decisions are
//! keyed by a fingerprint of the file's contents instead: size + the first
//! 64 KiB (which contains the EXIF block: capture time, shutter count...) +
//! the last 16 KiB. Fast even over a USB SD reader.

use std::io::{Read, Seek, SeekFrom};

use xxhash_rust::xxh3::Xxh3;

use crate::error::Result;

const HEAD: u64 = 64 * 1024;
const TAIL: u64 = 16 * 1024;

pub fn fingerprint<R: Read + Seek>(r: &mut R, size: u64) -> Result<String> {
    let mut h = Xxh3::new();
    h.update(&size.to_le_bytes());

    r.seek(SeekFrom::Start(0))?;
    let mut head = Vec::with_capacity(HEAD as usize);
    r.by_ref().take(HEAD).read_to_end(&mut head)?;
    h.update(&head);

    if size > HEAD + TAIL {
        r.seek(SeekFrom::Start(size - TAIL))?;
        let mut tail = Vec::with_capacity(TAIL as usize);
        r.by_ref().take(TAIL).read_to_end(&mut tail)?;
        h.update(&tail);
    }

    r.seek(SeekFrom::Start(0))?;
    Ok(format!("{:032x}", h.digest128()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn same_bytes_same_fingerprint_different_bytes_differ() {
        let a = vec![7u8; 200_000];
        let mut b = a.clone();
        b[199_000] = 8; // in the tail region (last 16 KiB)
        let fa = fingerprint(&mut Cursor::new(&a), a.len() as u64).unwrap();
        let fa2 = fingerprint(&mut Cursor::new(&a), a.len() as u64).unwrap();
        let fb = fingerprint(&mut Cursor::new(&b), b.len() as u64).unwrap();
        assert_eq!(fa, fa2);
        assert_ne!(fa, fb);
    }
}
