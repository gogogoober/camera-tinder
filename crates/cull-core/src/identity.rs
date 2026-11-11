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
    r.seek(SeekFrom::Start(0))?;
    let mut head = Vec::with_capacity(HEAD as usize);
    r.by_ref().take(HEAD).read_to_end(&mut head)?;

    let mut tail = Vec::new();
    if size > HEAD + TAIL {
        r.seek(SeekFrom::Start(size - TAIL))?;
        tail.reserve(TAIL as usize);
        r.by_ref().take(TAIL).read_to_end(&mut tail)?;
    }

    r.seek(SeekFrom::Start(0))?;
    Ok(from_parts(size, &head, &tail))
}

fn from_parts(size: u64, head: &[u8], tail: &[u8]) -> String {
    let mut h = Xxh3::new();
    h.update(&size.to_le_bytes());
    h.update(head);
    h.update(tail);
    format!("{:032x}", h.digest128())
}

/// The same fingerprint, computed while a file streams past once (offload copies read each card file once).
pub struct StreamingFingerprint {
    size: u64,
    pos: u64,
    head: Vec<u8>,
    tail: Vec<u8>,
}

impl StreamingFingerprint {
    /// `size`: the file's listed size. If more or fewer bytes arrive, `finish` won't match the real fingerprint.
    pub fn new(size: u64) -> Self {
        Self { size, pos: 0, head: Vec::with_capacity(HEAD as usize), tail: Vec::with_capacity(TAIL as usize) }
    }

    pub fn update(&mut self, buf: &[u8]) {
        let start = self.pos;
        let end = start + buf.len() as u64;
        if start < HEAD {
            let n = (HEAD.min(end) - start) as usize;
            self.head.extend_from_slice(&buf[..n]);
        }
        if self.size > HEAD + TAIL {
            let tail_start = self.size - TAIL;
            if end > tail_start && start < self.size {
                let from = tail_start.max(start) - start;
                let to = self.size.min(end) - start;
                self.tail.extend_from_slice(&buf[from as usize..to as usize]);
            }
        }
        self.pos = end;
    }

    pub fn finish(&self) -> String {
        from_parts(self.size, &self.head, &self.tail)
    }
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

    #[test]
    fn streaming_matches_for_any_chunking() {
        for len in [0usize, 10, 64 * 1024, 80 * 1024, 80 * 1024 + 1, 300_000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let want = fingerprint(&mut Cursor::new(&data), len as u64).unwrap();
            for chunk in [1usize, 7, 4096, 70_000, 1 << 20] {
                let mut s = StreamingFingerprint::new(len as u64);
                for c in data.chunks(chunk) {
                    s.update(c);
                }
                assert_eq!(s.finish(), want, "len {len} chunk {chunk}");
            }
        }
    }
}
