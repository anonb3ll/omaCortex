//! Sensors. Each reads one local source and reports readings to the recorder.

pub mod files;
pub mod hyprland;
pub mod media;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Sleep up to `secs`, waking early when `stop` is set.
pub fn wait(stop: &AtomicBool, secs: f64) {
    let mut left = secs;
    while left > 0.0 && !stop.load(Ordering::Relaxed) {
        let step = left.min(0.25);
        std::thread::sleep(Duration::from_secs_f64(step));
        left -= step;
    }
}
