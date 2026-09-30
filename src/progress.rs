use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_secs(1);
const REDRAW: Duration = Duration::from_millis(200);

static STOP: AtomicBool = AtomicBool::new(false);

pub fn interrupt() -> bool {
    STOP.swap(true, Ordering::SeqCst)
}

pub fn stopped() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub(crate) struct Progress {
    label: &'static str,
    total: u64,
    start: Instant,
    last: Instant,
    shown: bool,
    enabled: bool,
}

impl Progress {
    pub(crate) fn new(label: &'static str, total: u64) -> Progress {
        let now = Instant::now();
        Progress {
            label,
            total,
            start: now,
            last: now,
            shown: false,
            enabled: !cfg!(target_arch = "wasm32") && total > 1 && io::stderr().is_terminal(),
        }
    }

    pub(crate) fn tick(&mut self, done: u64) {
        if !self.enabled || done == 0 {
            return;
        }
        let now = Instant::now();
        let elapsed = now - self.start;
        if elapsed < QUIET || now - self.last < REDRAW {
            return;
        }
        self.last = now;
        self.shown = true;
        let left = elapsed.as_secs_f64() * (self.total.saturating_sub(done)) as f64 / done as f64;
        let mut error = io::stderr().lock();
        write!(
            error,
            "\r{}: {done} of {}, {:.1} s, about {left:.0} s left, Ctrl+C stops   ",
            self.label,
            self.total,
            elapsed.as_secs_f64()
        )
        .and_then(|()| error.flush())
        .ok();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.shown {
            let mut error = io::stderr().lock();
            write!(error, "\r{}\r", " ".repeat(90))
                .and_then(|()| error.flush())
                .ok();
        }
    }
}
