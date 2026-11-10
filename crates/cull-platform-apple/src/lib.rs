//! Apple platform adapters: Calendar via EventKit, photo analysis via Vision, previews via ImageIO (macOS).
//!
//! iOS reuses the same EventKit API; only the entitlement/Info.plist differs.

#[cfg(target_os = "macos")]
mod calendar;
#[cfg(target_os = "macos")]
pub use calendar::{CalendarAccess, EventKitCalendar};

/// Calendar permission state, platform-neutral so the UI can show the right prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    NotDetermined,
    Denied,
    Granted,
}

/// Photo analysis engines backed by Apple Vision (see `docs/photo-analysis.md`).
#[cfg(target_os = "macos")]
pub mod vision;

/// Fast preview maker (decode at reduced size + rotate + scale in one ImageIO call).
#[cfg(target_os = "macos")]
pub mod imageio;
