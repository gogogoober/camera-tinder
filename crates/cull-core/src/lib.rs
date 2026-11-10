//! cull-core: all of Darkroom's logic, with no UI or platform code.
//!
//! Platforms plug in through the traits in [`ports`] (where photos come from,
//! where keepers go, how progress is reported). Any UI — Tauri today, SwiftUI
//! via UniFFI tomorrow — talks only to [`CullerService`].

pub mod analysis;
pub mod api;
pub mod errlog;
pub mod error;
pub mod identity;
pub mod metadata;
pub mod model;
pub mod moments;
pub mod ports;
pub mod preview;
pub mod scan;
pub mod store;
pub mod suggest;
pub mod triage;
mod worker;

pub use api::CullerService;
pub use error::{CoreError, Result};
pub use model::*;
pub use ports::*;
pub use suggest::CalendarEvent;
