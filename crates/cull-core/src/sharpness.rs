//! Blur detection with plain image maths (no AI).
//!
//! Method: downscale, convert to grey, apply a 4-neighbour Laplacian and take
//! the variance of the response. Sharp images have strong edges, so the
//! variance is high; blurry images are smooth, so it is low. The variance is
//! squashed to 0–100 so the UI can show a simple meter.
//!
//! Known weak spot: a genuinely sharp photo of a plain scene (clear sky, fog)
//! also scores low. That is why the UI only *suggests* rejects.

use std::io::{BufRead, Seek};

use image::{DynamicImage, GrayImage, ImageDecoder, ImageReader};

/// Long edge the image is shrunk to before measuring, so scores are
/// comparable across 12 MP and 45 MP cameras and the maths stays fast.
pub const MEASURE_EDGE: u32 = 640;
/// Variance at which the score reads 50.
const HALF_SCORE_VARIANCE: f64 = 150.0;

pub fn score<R: BufRead + Seek>(reader: R) -> Option<u8> {
    let mut decoder = ImageReader::new(reader).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(score_image(&img))
}

pub fn score_image(img: &DynamicImage) -> u8 {
    let small = if img.width() > MEASURE_EDGE || img.height() > MEASURE_EDGE {
        img.thumbnail(MEASURE_EDGE, MEASURE_EDGE)
    } else {
        img.clone()
    };
    let grey = small.to_luma8();
    if grey.width() < 3 || grey.height() < 3 {
        return 0;
    }
    squash(laplacian_variance(&grey, 0, 0, grey.width(), grey.height()))
}

/// Variance of the 4-neighbour Laplacian inside `x..x+w`, `y..y+h` of `grey`.
/// Shared with `analysis::blur` and `analysis::subject_focus`.
pub fn laplacian_variance(grey: &GrayImage, x: u32, y: u32, w: u32, h: u32) -> f64 {
    let (iw, ih) = (grey.width() as usize, grey.height() as usize);
    let (x0, y0) = ((x as usize).max(1), (y as usize).max(1));
    let (x1, y1) = (((x + w) as usize).min(iw - 1), ((y + h) as usize).min(ih - 1));
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let px = grey.as_raw();
    let (mut sum, mut sum_sq, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let c = px[yy * iw + xx] as f64;
            let lap = px[(yy - 1) * iw + xx] as f64 + px[(yy + 1) * iw + xx] as f64 + px[yy * iw + xx - 1] as f64 + px[yy * iw + xx + 1] as f64 - 4.0 * c;
            sum += lap;
            sum_sq += lap * lap;
            n += 1.0;
        }
    }
    let mean = sum / n;
    (sum_sq / n - mean * mean).max(0.0)
}

/// Map a Laplacian variance to the 0–100 sharpness scale.
pub fn squash(variance: f64) -> u8 {
    (100.0 * variance / (variance + HALF_SCORE_VARIANCE)).round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    fn checker(blur: bool) -> DynamicImage {
        let img = ImageBuffer::from_fn(400, 300, |x, y| Luma([if (x / 4 + y / 4) % 2 == 0 { 30u8 } else { 220u8 }]));
        let img = DynamicImage::ImageLuma8(img);
        if blur { img.blur(6.0) } else { img }
    }

    #[test]
    fn blur_scores_lower_than_sharp() {
        let sharp = score_image(&checker(false));
        let soft = score_image(&checker(true));
        assert!(sharp > 60, "sharp = {sharp}");
        assert!(soft < crate::model::SOFT_THRESHOLD, "soft = {soft}");
    }

    #[test]
    fn flat_image_is_zero() {
        let flat = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(100, 100, Luma([128u8])));
        assert_eq!(score_image(&flat), 0);
    }
}
