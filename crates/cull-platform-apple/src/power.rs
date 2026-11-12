//! Keep the Mac awake during long work (an offload) without starting another process: an `NSProcessInfo`
//! activity that disables idle system sleep (the display may still sleep). Sandbox-safe, unlike `caffeinate`.

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// Idle system sleep stays off while this lives; ended on drop. Create and drop it on the same thread.
pub struct KeepAwake(Retained<ProtocolObject<dyn NSObjectProtocol>>);

impl KeepAwake {
    pub fn new(reason: &str) -> Self {
        let options = NSActivityOptions::UserInitiated | NSActivityOptions::IdleSystemSleepDisabled;
        Self(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &NSString::from_str(reason)))
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // SAFETY: the token is the one `beginActivityWithOptions:reason:` returned.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn begins_and_ends_an_activity() {
        drop(super::KeepAwake::new("test"));
    }
}
