//! Preview maker backed by Apple ImageIO (macOS; the same API exists on iOS).
//!
//! `CGImageSourceCreateThumbnailAtIndex` decodes a JPEG straight at a reduced size (the
//! decoder skips the detail it would throw away), applies the EXIF orientation and scales
//! to the requested long edge, all in one call. That replaces the portable path's full-size
//! decode, rotate and downscale. Smaller sizes are drawn from that first image (no second
//! decode), and every size is written with ImageIO's JPEG encoder.
//!
//! Plain C APIs, declared here directly: only a dozen functions are needed.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use cull_core::ports::PreviewMaker;
use cull_core::{CoreError, Result};

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorNull: CFTypeRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRelease(cf: CFTypeRef);
    fn CFDataCreateWithBytesNoCopy(alloc: CFTypeRef, bytes: *const u8, len: CFIndex, dealloc: CFTypeRef) -> CFTypeRef;
    fn CFDataCreateMutable(alloc: CFTypeRef, capacity: CFIndex) -> CFTypeRef;
    fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFNumberCreate(alloc: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const u8, encoding: u32) -> CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImageSourceShouldCacheImmediately: CFTypeRef;
    static kCGImageDestinationLossyCompressionQuality: CFTypeRef;
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageDestinationCreateWithData(data: CFTypeRef, uti: CFTypeRef, count: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageDestinationAddImage(dest: CFTypeRef, image: CFTypeRef, properties: CFTypeRef);
    fn CGImageDestinationFinalize(dest: CFTypeRef) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: CFTypeRef;
    fn CGImageGetWidth(image: CFTypeRef) -> usize;
    fn CGImageGetHeight(image: CFTypeRef) -> usize;
    fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bits: usize, row_bytes: usize, space: CFTypeRef, info: u32) -> CFTypeRef;
    fn CGBitmapContextCreateImage(ctx: CFTypeRef) -> CFTypeRef;
    fn CGContextSetInterpolationQuality(ctx: CFTypeRef, quality: i32);
    fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, image: CFTypeRef);
}

const K_CF_NUMBER_SINT64: CFIndex = 4;
const K_CF_NUMBER_FLOAT64: CFIndex = 6;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CG_IMAGE_ALPHA_NONE_SKIP_LAST: u32 = 5;
const K_CG_INTERPOLATION_HIGH: i32 = 3;
/// ImageIO's scale runs higher than libjpeg's: 0.85 made previews ~50% bigger than the portable
/// encoder at 85. 0.7 keeps the preview cache about the same size (~0.6 MB per photo).
const JPEG_QUALITY: f64 = 0.7;

/// An owned Core Foundation object, released on drop.
struct Cf(NonNull<c_void>);

impl Cf {
    fn new(ptr: CFTypeRef, what: &str) -> Result<Cf> {
        NonNull::new(ptr as *mut c_void).map(Cf).ok_or_else(|| CoreError::Other(format!("ImageIO: {what} failed")))
    }
    fn ptr(&self) -> CFTypeRef {
        self.0.as_ptr()
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.ptr()) }
    }
}

fn number_i64(v: i64) -> Result<Cf> {
    Cf::new(unsafe { CFNumberCreate(ptr::null(), K_CF_NUMBER_SINT64, (&v as *const i64).cast()) }, "number")
}

fn number_f64(v: f64) -> Result<Cf> {
    Cf::new(unsafe { CFNumberCreate(ptr::null(), K_CF_NUMBER_FLOAT64, (&v as *const f64).cast()) }, "number")
}

fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Result<Cf> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
    Cf::new(
        unsafe {
            CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                pairs.len() as CFIndex,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
        },
        "dictionary",
    )
}

/// Decode `jpeg` at (about) `edge` px on the long side, upright. Never upscales.
fn thumbnail(jpeg: &[u8], edge: u32) -> Result<Cf> {
    // The bytes outlive every object made from them in `render`, so ImageIO can read them in place.
    let data = Cf::new(unsafe { CFDataCreateWithBytesNoCopy(ptr::null(), jpeg.as_ptr(), jpeg.len() as CFIndex, kCFAllocatorNull) }, "wrap bytes")?;
    let source = Cf::new(unsafe { CGImageSourceCreateWithData(data.ptr(), ptr::null()) }, "open image")?;
    let max = number_i64(edge as i64)?;
    let options = unsafe {
        dictionary(&[
            // Always decode the real image: an embedded EXIF thumbnail is only ~160 px.
            (kCGImageSourceCreateThumbnailFromImageAlways, kCFBooleanTrue),
            (kCGImageSourceCreateThumbnailWithTransform, kCFBooleanTrue),
            (kCGImageSourceShouldCacheImmediately, kCFBooleanTrue),
            (kCGImageSourceThumbnailMaxPixelSize, max.ptr()),
        ])?
    };
    Cf::new(unsafe { CGImageSourceCreateThumbnailAtIndex(source.ptr(), 0, options.ptr()) }, "decode")
}

/// Draw `image` scaled so its long edge is at most `edge` (sRGB, high-quality interpolation).
fn scaled(image: &Cf, edge: u32) -> Result<Option<Cf>> {
    let (w, h) = unsafe { (CGImageGetWidth(image.ptr()), CGImageGetHeight(image.ptr())) };
    let long = w.max(h);
    if long <= edge as usize {
        return Ok(None);
    }
    let scale = edge as f64 / long as f64;
    let (tw, th) = (((w as f64 * scale).round() as usize).max(1), ((h as f64 * scale).round() as usize).max(1));
    let space = Cf::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) }, "colour space")?;
    let ctx = Cf::new(unsafe { CGBitmapContextCreate(ptr::null_mut(), tw, th, 8, 0, space.ptr(), K_CG_IMAGE_ALPHA_NONE_SKIP_LAST) }, "bitmap")?;
    unsafe {
        CGContextSetInterpolationQuality(ctx.ptr(), K_CG_INTERPOLATION_HIGH);
        CGContextDrawImage(ctx.ptr(), CGRect { x: 0.0, y: 0.0, width: tw as f64, height: th as f64 }, image.ptr());
    }
    Cf::new(unsafe { CGBitmapContextCreateImage(ctx.ptr()) }, "scale").map(Some)
}

fn encode_jpeg(image: &Cf) -> Result<Vec<u8>> {
    let out = Cf::new(unsafe { CFDataCreateMutable(ptr::null(), 0) }, "buffer")?;
    let uti = Cf::new(unsafe { CFStringCreateWithCString(ptr::null(), c"public.jpeg".as_ptr().cast(), K_CF_STRING_ENCODING_UTF8) }, "type")?;
    let dest = Cf::new(unsafe { CGImageDestinationCreateWithData(out.ptr(), uti.ptr(), 1, ptr::null()) }, "encoder")?;
    let quality = number_f64(JPEG_QUALITY)?;
    let props = unsafe { dictionary(&[(kCGImageDestinationLossyCompressionQuality, quality.ptr())])? };
    unsafe {
        CGImageDestinationAddImage(dest.ptr(), image.ptr(), props.ptr());
        if !CGImageDestinationFinalize(dest.ptr()) {
            return Err(CoreError::Other("ImageIO: encode failed".into()));
        }
        let len = CFDataGetLength(out.ptr()) as usize;
        Ok(std::slice::from_raw_parts(CFDataGetBytePtr(out.ptr()), len).to_vec())
    }
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

