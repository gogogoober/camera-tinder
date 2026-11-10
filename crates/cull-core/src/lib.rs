//! cull-core: all of Camera Tinder's logic, with no UI or platform code.
//!
//! Platforms plug in through the traits in [`ports`] (where photos come from,
//! where keepers go, how progress is reported). Any UI — Tauri today, SwiftUI
//! via UniFFI tomorrow — talks only to [`CullerService`].

pub mod api;
pub mod error;
pub mod identity;
pub mod metadata;
pub mod model;
pub mod ports;
pub mod preview;
pub mod scan;
pub mod store;

pub use api::CullerService;
pub use error::{CoreError, Result};
pub use model::*;
pub use ports::*;
