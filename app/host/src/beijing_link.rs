use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::dashscope::AsrError;

/// 一次北京区调用对连接故障位的影响。限流说明已经连上；本地音频和协议错误不改故障位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeijingCallOutcome {
    Reached,
    Fault,
    Unchanged,
}

pub fn beijing_call_outcome(error: AsrError) -> BeijingCallOutcome {
    match error {
        AsrError::Rejected | AsrError::Unavailable => BeijingCallOutcome::Fault,
        AsrError::RateLimited => BeijingCallOutcome::Reached,
        AsrError::InvalidAudio | AsrError::Protocol => BeijingCallOutcome::Unchanged,
    }
}

/// 北京区密钥或连通性。由 Host 持有一份，识别线程和信箱循环共用。
#[derive(Debug, Default)]
pub struct BeijingLink {
    key_missing: AtomicBool,
    call_fault: AtomicBool,
}

impl BeijingLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn note_key_installed(&self, installed: bool) {
        self.key_missing.store(!installed, Ordering::Release);
    }

    pub fn note_call(&self, outcome: BeijingCallOutcome) {
        match outcome {
            BeijingCallOutcome::Reached => self.call_fault.store(false, Ordering::Release),
            BeijingCallOutcome::Fault => self.call_fault.store(true, Ordering::Release),
            BeijingCallOutcome::Unchanged => {}
        }
    }

    pub fn service_fault(&self) -> bool {
        self.key_missing.load(Ordering::Acquire) || self.call_fault.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failures_latch_until_beijing_answers() {
        let link = BeijingLink::new();
        assert!(!link.service_fault());

        link.note_key_installed(false);
        assert!(link.service_fault());
        link.note_call(BeijingCallOutcome::Reached);
        assert!(link.service_fault());

        link.note_key_installed(true);
        assert!(!link.service_fault());
        link.note_call(beijing_call_outcome(AsrError::Unavailable));
        assert!(link.service_fault());
        link.note_call(beijing_call_outcome(AsrError::InvalidAudio));
        assert!(link.service_fault());
        link.note_call(beijing_call_outcome(AsrError::RateLimited));
        assert!(!link.service_fault());

        link.note_call(beijing_call_outcome(AsrError::Rejected));
        assert!(link.service_fault());
        link.note_call(beijing_call_outcome(AsrError::Protocol));
        assert!(link.service_fault());
        link.note_call(BeijingCallOutcome::Reached);
        assert!(!link.service_fault());
    }
}
