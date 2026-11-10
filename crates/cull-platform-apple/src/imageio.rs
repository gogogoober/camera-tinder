//! Preview maker backed by Apple ImageIO (macOS; the same API exists on iOS).
//!
//! `CGImageSourceCreateThumbnailAtIndex` decodes a JPEG straight at a reduced size (the
//! decoder skips the detail it would throw away), applies the EXIF orientation and scales
//! to the requested long edge, all in one call. That replaces the portable path's full-size
//! decode, rotate and downscale. Smaller sizes are drawn from that first image (no second
//! decode), and every size is written with ImageIO's JPEG encoder.
//!
//! Uses the typed `objc2` bindings (same family as the Vision code): Core Foundation objects
//! are released automatically, and the photo's bytes are copied into ImageIO's own buffer
//! rather than lent to it, so nothing depends on how long ImageIO keeps a reference.

use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGInterpolationQuality, kCGColorSpaceSRGB};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};

use cull_core::ports::PreviewMaker;
use cull_core::{CoreError, Result};

/// ImageIO's scale runs higher than libjpeg's: 0.85 made previews ~50% bigger than the portable
/// encoder at 85. 0.7 keeps the preview cache about the same size (~0.6 MB per photo).
const JPEG_QUALITY: f64 = 0.7;

fn failed(what: &str) -> CoreError {
    CoreError::Other(format!("ImageIO: {what} failed"))
}

fn options(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let (keys, values): (Vec<&CFString>, Vec<&CFType>) = pairs.iter().copied().unzip();
    CFDictionary::from_slices(&keys, &values)
}

/// Decode `jpeg` at (about) `edge` px on the long side, upright. Never upscales.
fn thumbnail(jpeg: &[u8], edge: u32) -> Result<CFRetained<CGImage>> {
    let data = CFData::from_bytes(jpeg); // a copy: ImageIO owns its input
    let max = CFNumber::new_i64(edge as i64);
    // SAFETY: the keys are ImageIO's own constants and each value has the type ImageIO documents for it.
    unsafe {
        let source = CGImageSource::with_data(&data, None).ok_or_else(|| failed("open image"))?;
        let opts = options(&[
            // Always decode the real image: an embedded EXIF thumbnail is only ~160 px.
            (kCGImageSourceCreateThumbnailFromImageAlways, CFBoolean::new(true)),
            (kCGImageSourceCreateThumbnailWithTransform, CFBoolean::new(true)),
            (kCGImageSourceShouldCacheImmediately, CFBoolean::new(true)),
            (kCGImageSourceThumbnailMaxPixelSize, &max),
        ]);
        source.thumbnail_at_index(0, Some(opts.as_ref())).ok_or_else(|| failed("decode"))
    }
}

/// Draw `image` scaled so its long edge is at most `edge` (sRGB, high-quality interpolation).
fn scaled(image: &CGImage, edge: u32) -> Result<Option<CFRetained<CGImage>>> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let long = w.max(h);
    if long <= edge as usize {
        return Ok(None);
    }
    let scale = edge as f64 / long as f64;
    let (tw, th) = (((w as f64 * scale).round() as usize).max(1), ((h as f64 * scale).round() as usize).max(1));
    // SAFETY: sRGB is a constant name; a null data pointer asks Core Graphics to allocate the bitmap.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or_else(|| failed("colour space"))?;
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), tw, th, 8, 0, Some(&space), CGImageAlphaInfo::NoneSkipLast.0) }.ok_or_else(|| failed("bitmap"))?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(tw as f64, th as f64)), Some(image));
    CGBitmapContextCreateImage(Some(&ctx)).map(Some).ok_or_else(|| failed("scale"))
}

fn encode_jpeg(image: &CGImage) -> Result<Vec<u8>> {
    let out = CFMutableData::new(None, 0).ok_or_else(|| failed("buffer"))?;
    let quality = CFNumber::new_f64(JPEG_QUALITY);
    // SAFETY: "public.jpeg" is a valid type identifier and the quality value is a CFNumber, as documented.
    unsafe {
        let dest = CGImageDestination::with_data(&out, &CFString::from_static_str("public.jpeg"), 1, None).ok_or_else(|| failed("encoder"))?;
        let props = options(&[(kCGImageDestinationLossyCompressionQuality, &quality)]);
        dest.add_image(image, Some(props.as_ref()));
        if !dest.finalize() {
            return Err(failed("encode"));
        }
    }
    Ok(out.to_vec())
}

/// See the module docs. Thread-safe: each call uses its own ImageIO objects.
pub struct ImageIoPreviewMaker;

impl PreviewMaker for ImageIoPreviewMaker {
    fn implementation(&self) -> &'static str {
        "apple.imageio.v1"
    }

    fn render(&self, jpeg: &[u8], edges: &[u32]) -> Result<Vec<Vec<u8>>> {
        let Some((&first, rest)) = edges.split_first() else { return Ok(Vec::new()) };
        let mut image = thumbnail(jpeg, first)?;
        let mut out = vec![encode_jpeg(&image)?];
        for &edge in rest {
            if let Some(smaller) = scaled(&image, edge)? {
                image = smaller;
            }
            out.push(encode_jpeg(&image)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, ImageBuffer, Rgb};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img).unwrap();
        out
    }

    #[test]
    fn renders_each_size_largest_first_without_upscaling() {
        let out = ImageIoPreviewMaker.render(&jpeg(3000, 2000), &[2048, 256]).unwrap();
        let dims: Vec<_> = out.iter().map(|b| image::load_from_memory(b).unwrap().dimensions()).collect();
        assert_eq!(dims, vec![(2048, 1365), (256, 171)]);
        // The preview cache only trusts files with JPEG start and end markers (cut-short files are rebuilt).
        assert!(out.iter().all(|b| b.starts_with(&[0xFF, 0xD8]) && b.ends_with(&[0xFF, 0xD9])));

        let small = ImageIoPreviewMaker.render(&jpeg(800, 600), &[2048, 256]).unwrap();
        assert_eq!(image::load_from_memory(&small[0]).unwrap().dimensions(), (800, 600));
    }

    #[test]
    fn rejects_garbage() {
        assert!(ImageIoPreviewMaker.render(b"not a jpeg", &[2048]).is_err());
    }
}

