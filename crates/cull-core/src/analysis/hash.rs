//! Perceptual hash: a 64-bit fingerprint of what the photo *looks* like.
//!
//! Unlike the content fingerprint in `identity.rs` (identical bytes only), two photos that
//! look almost the same — a burst, a re-save, a slight exposure change — get hashes that
//! differ in only a few bits. Compare with [`PerceptualHash::distance`] (0–64).
//!
//! Rust implementation: dHash (difference hash). Shrink to 9×8 grey and record, for each
//! row, whether each pixel is brighter than its right neighbour. Very fast and robust to
//! exposure and scaling; less robust to reframing — that's what `similarity` is for.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::image::AnalysisImage;
use crate::error::{CoreError, Result};

/// 64-bit hash. Serialised as 16 hex characters (JavaScript numbers can't hold 64 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PerceptualHash(pub u64);

impl PerceptualHash {
    /// Number of differing bits, 0 (same look) – 64.
    pub fn distance(&self, other: &PerceptualHash) -> u32 {
        (self.0 ^ other.0).count_ones()
    }
}

impl Serialize for PerceptualHash {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{:016x}", self.0))
    }
}

impl<'de> Deserialize<'de> for PerceptualHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        u64::from_str_radix(&s, 16).map(PerceptualHash).map_err(serde::de::Error::custom)
    }
}

pub trait PerceptualHasher: Send + Sync {
    fn implementation(&self) -> &'static str;
    fn hash(&self, img: &AnalysisImage) -> Result<PerceptualHash>;
}

/// Hash distance at or below which two photos look nearly identical.
pub const NEAR_DUPLICATE_BITS: u32 = 10;

pub struct DHash;

impl PerceptualHasher for DHash {
    fn implementation(&self) -> &'static str {
        "rust.dhash.v1"
    }

    fn hash(&self, img: &AnalysisImage) -> Result<PerceptualHash> {
        let grey = img.working_grey()?;
        if grey.width() < 9 || grey.height() < 8 {
            return Err(CoreError::Other("image too small to hash".into()));
        }
        let small = image::imageops::resize(grey, 9, 8, image::imageops::FilterType::Triangle);
        let mut bits = 0u64;
        for y in 0..8 {
            for x in 0..8 {
                bits <<= 1;
                if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                    bits |= 1;
                }
            }
        }
        Ok(PerceptualHash(bits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn scene(shift: u8, flip: bool) -> AnalysisImage {
        let img = ImageBuffer::from_fn(320, 240, |x, y| {
            let x = if flip { 319 - x } else { x };
            let v = ((x as f32 / 30.0).sin() * 60.0 + (y as f32 / 25.0).cos() * 60.0 + 128.0) as u8;
            Rgb([v.saturating_add(shift), v.saturating_add(shift), v])
        });
        AnalysisImage::from_image(DynamicImage::ImageRgb8(img))
    }

    #[test]
    fn near_and_far() {
        let a = DHash.hash(&scene(0, false)).unwrap();
        let brighter = DHash.hash(&scene(20, false)).unwrap();
        let other = DHash.hash(&scene(0, true)).unwrap();
        assert!(a.distance(&brighter) <= NEAR_DUPLICATE_BITS, "{}", a.distance(&brighter));
        assert!(a.distance(&other) > NEAR_DUPLICATE_BITS, "{}", a.distance(&other));
    }

    #[test]
    fn serde_roundtrip() {
        let h = PerceptualHash(u64::MAX - 5);
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(s, "\"fffffffffffffffa\"");
        assert_eq!(serde_json::from_str::<PerceptualHash>(&s).unwrap(), h);
    }
}
