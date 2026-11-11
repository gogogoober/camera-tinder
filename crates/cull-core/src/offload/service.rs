//! `CullerService` methods for offload. One plan is kept between calls so folder choices re-lay it out
//! without reading the card or drive again; starting a run uses that plan.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::plan::{FolderChoice, Gather, OffloadPlan, PlanInput, gather, layout};
use super::run::{OffloadReport, RunArgs, run};
use crate::api::CullerService;
use crate::error::{CoreError, Result};
use crate::ports::{EventSink, Library, MediaSource};

struct Planned {
    input: PlanInput,
    card: Arc<dyn MediaSource>,
    lib: Arc<dyn Library>,
}

#[derive(Default)]
pub struct OffloadState {
    current: Mutex<Option<Planned>>,
    /// One plan or run at a time.
    busy: Mutex<()>,
    stop: AtomicBool,
}

impl CullerService {
    /// Read the card and the drive and propose where every new photo and video goes. `library` reads
    /// the drive from the picked folder; `lib` writes there. Nothing is written.
    pub fn offload_plan(
        &self,
        card: Arc<dyn MediaSource>,
        card_root: String,
        library: Arc<dyn MediaSource>,
        lib: Arc<dyn Library>,
        full_check: bool,
        sink: &dyn EventSink,
    ) -> Result<OffloadPlan> {
        let _busy = self.offload.busy.try_lock().map_err(|_| CoreError::Other("a backup is already running".into()))?;
        self.offload.stop.store(false, Ordering::Relaxed);
        let calendar = self.calendar.read().unwrap().clone();
        let input = gather(Gather {
            card: card.as_ref(),
            card_root,
            library: library.as_ref(),
            lib: lib.as_ref(),
            store: &self.store,
            calendar: calendar.as_deref(),
            sink,
            cancel: &self.offload.stop,
            full_check,
        })?;
        let (_, plan) = layout(&input, &[]);
        *self.offload.current.lock().unwrap() = Some(Planned { input, card, lib });
        Ok(plan)
    }

    /// The current plan with these folder choices (renames, trips split by month). Reads nothing.
    pub fn offload_layout(&self, choices: &[FolderChoice]) -> Result<OffloadPlan> {
        let cur = self.offload.current.lock().unwrap();
        let p = cur.as_ref().ok_or_else(|| CoreError::Other("no backup plan yet".into()))?;
        Ok(layout(&p.input, choices).1)
    }

    /// Copy the current plan's new files. Blocks until done, stopped, or the card or drive is gone.
    /// The plan is used up: plan again before another run.
    pub fn offload_start(&self, choices: &[FolderChoice], sink: &dyn EventSink) -> Result<OffloadReport> {
        let _busy = self.offload.busy.try_lock().map_err(|_| CoreError::Other("a backup is already running".into()))?;
        let p = self.offload.current.lock().unwrap().take().ok_or_else(|| CoreError::Other("no backup plan yet".into()))?;
        self.offload.stop.store(false, Ordering::Relaxed);
        let (planned, _) = layout(&p.input, choices);
        run(RunArgs {
            input: &p.input,
            planned: &planned,
            card: p.card.as_ref(),
            lib: p.lib.as_ref(),
            store: &self.store,
            sink,
            stop: &self.offload.stop,
        })
    }

    /// Stop planning or copying. Files already verified stay; the one being copied is removed.
    pub fn offload_stop(&self) {
        self.offload.stop.store(true, Ordering::Relaxed);
    }
}
