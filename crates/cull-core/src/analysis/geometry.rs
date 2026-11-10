//! Normalised geometry shared by analyzers.
//!
//! All coordinates are fractions (0–1) of the upright image with the origin at the
//! **top-left**, like CSS. Platform engines with other conventions (Apple Vision uses a
//! bottom-left origin) convert before returning.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    pub fn center(&self) -> NormPoint {
        NormPoint { x: self.x + self.width / 2.0, y: self.y + self.height / 2.0 }
    }

    /// True when the rectangle reaches within `margin` of any image edge (or past it).
    pub fn touches_edge(&self, margin: f32) -> bool {
        self.x <= margin || self.y <= margin || self.x + self.width >= 1.0 - margin || self.y + self.height >= 1.0 - margin
    }

    /// Intersection over union, 0–1.
    pub fn iou(&self, other: &NormRect) -> f32 {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// Clamp to the image and convert to whole pixels `(x, y, w, h)` for a `w`×`h` image.
    pub fn to_pixels(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x0 = (self.x.clamp(0.0, 1.0) * w as f32).round() as u32;
        let y0 = (self.y.clamp(0.0, 1.0) * h as f32).round() as u32;
        let x1 = ((self.x + self.width).clamp(0.0, 1.0) * w as f32).round() as u32;
        let y1 = ((self.y + self.height).clamp(0.0, 1.0) * h as f32).round() as u32;
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// Grow by `factor` around the centre (e.g. 1.2 = 20% larger), clamped to the image.
    pub fn expanded(&self, factor: f32) -> NormRect {
        let c = self.center();
        let (w, h) = (self.width * factor, self.height * factor);
        let x = (c.x - w / 2.0).max(0.0);
        let y = (c.y - h / 2.0).max(0.0);
        NormRect { x, y, width: w.min(1.0 - x), height: h.min(1.0 - y) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_iou_pixels() {
        let r = NormRect::new(0.1, 0.1, 0.2, 0.2);
        assert!(!r.touches_edge(0.01));
        assert!(NormRect::new(0.9, 0.4, 0.1, 0.1).touches_edge(0.01));
        assert!((r.iou(&r) - 1.0).abs() < 1e-6);
        assert_eq!(r.iou(&NormRect::new(0.6, 0.6, 0.1, 0.1)), 0.0);
        assert_eq!(r.to_pixels(100, 50), (10, 5, 20, 10));
    }
}
