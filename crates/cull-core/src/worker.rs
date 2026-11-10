//! Background analysis: one long-lived thread that analyses every photo that still needs it.
//!
//! The scan wakes it after each saved batch (and when it ends); it also runs once at start, which
//! resumes unfinished work after a restart. What needs doing always comes from the database
//! ("which photos lack a current result"), never from events, so nothing is lost between the scan
//! and the worker. Stop is sticky: it lasts until [`CullerService::resume_analysis`] or a restart,
//! so new import batches don't quietly start analysis again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::analysis::{AnalyzeReport, AnalyzeRequest};
use crate::api::CullerService;
use crate::error::Result;
use crate::ports::EventSink;

/// Shared between the service and its worker thread.
pub(crate) struct AnalysisControl {
    /// "There may be work." Starts true so the first pass resumes unfinished work.
    wake: Mutex<bool>,
    cv: Condvar,
    /// Set by `stop_analysis`; checked before every analysis step.
    pub(crate) stop: AtomicBool,
    started: AtomicBool,
}

impl AnalysisControl {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self { wake: Mutex::new(true), cv: Condvar::new(), stop: AtomicBool::new(false), started: AtomicBool::new(false) })
    }

    fn wake(&self) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_one();
    }

    /// Wait until woken (or a second passes, so the thread notices the service is gone).
    fn wait(&self) -> bool {
        let mut woken = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        if !*woken {
            woken = self.cv.wait_timeout(woken, Duration::from_secs(1)).unwrap_or_else(PoisonError::into_inner).0;
        }
        std::mem::take(&mut *woken)
    }
}

impl CullerService {
    /// Start the background analysis thread. Call once, after the analyzers and preview maker are
    /// set; further calls do nothing. The thread ends when the service is dropped.
    pub fn start_analysis_worker(self: &Arc<Self>, sink: Arc<dyn EventSink>) {
        let control = self.analysis.clone();
        if control.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let svc = Arc::downgrade(self);
        let spawned = std::thread::Builder::new().name("analysis".into()).spawn(move || {
            loop {
                let woken = control.wait();
                let Some(svc) = svc.upgrade() else { return };
                if !woken || control.stop.load(Ordering::Relaxed) {
                    continue;
                }
                if let Err(e) = svc.analyze_pending(sink.as_ref()) {
                    eprintln!("background analysis failed: {e}");
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("could not start the analysis thread: {e}");
        }
    }

    /// Ask the worker to look for work now (the scan does this after every saved batch).
    pub fn wake_analysis(&self) {
        self.analysis.wake();
    }

    /// Stop analysing before the next step. Results already saved are kept. Stays stopped until
    /// `resume_analysis` or an app restart.
    pub fn stop_analysis(&self) {
        self.analysis.stop.store(true, Ordering::Relaxed);
    }

    /// Undo `stop_analysis` and continue where it left off.
    pub fn resume_analysis(&self) {
        self.analysis.stop.store(false, Ordering::Relaxed);
        self.wake_analysis();
    }

    pub fn analysis_stopped(&self) -> bool {
        self.analysis.stop.load(Ordering::Relaxed)
    }

    /// One pass over every photo of the session: analyse whatever lacks a current result.
    /// Photos whose cached preview is missing are skipped (reported in `failed`) until the next pass.
    pub fn analyze_pending(&self, sink: &dyn EventSink) -> Result<AnalyzeReport> {
        self.analyze(&AnalyzeRequest::default(), sink)
    }
}
