//! Offload: back up a card to a drive. See docs/offload.md.
//!
//! `plan::gather` reads the card and the drive, `plan::layout` decides folders (pure), and
//! `run::run` copies, reads back and records every file. The card is only ever read.

pub mod journal;
pub mod kind;
pub mod layout;
pub mod meta;
pub mod plan;
pub mod run;
pub mod service;

pub use journal::BrandBackup;
pub use kind::FileKind;
pub use plan::{FolderChoice, OffloadPlan, PlanFolder};
pub use run::{OffloadReport, StageTimes};
