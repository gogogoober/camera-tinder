//! Apple platform adapters. Today: Calendar via EventKit (macOS).
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
