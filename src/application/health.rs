//! Process readiness follows planner progress, rather than just an open HTTP socket.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_PLANNER_LAG: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(crate) struct Heartbeat(Arc<Progress>);

struct Progress {
    started: Instant,
    last_millis: AtomicU64,
}

impl Heartbeat {
    fn new() -> Self {
        Self(Arc::new(Progress {
            started: Instant::now(),
            last_millis: AtomicU64::new(0),
        }))
    }

    pub(crate) fn beat(&self) {
        self.0
            .last_millis
            .store(self.elapsed_millis(), Ordering::Release);
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.0.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn is_recent(&self) -> bool {
        let age = self
            .elapsed_millis()
            .saturating_sub(self.0.last_millis.load(Ordering::Acquire));
        age <= MAX_PLANNER_LAG.as_millis() as u64
    }
}

#[derive(Clone)]
pub(crate) struct Health {
    planner: Heartbeat,
    stopping: Arc<AtomicBool>,
}

impl Health {
    pub(crate) fn new() -> Self {
        Self {
            planner: Heartbeat::new(),
            stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn heartbeat(&self) -> Heartbeat {
        self.planner.clone()
    }

    pub(crate) fn is_ready(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.planner.is_recent()
    }

    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
