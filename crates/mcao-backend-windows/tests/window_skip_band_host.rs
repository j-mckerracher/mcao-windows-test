//! Host-runnable pins for the second Ctrl+C skip band at the production escalation wait.
//!
//! Like `window_host.rs`, this file includes `src/window.rs` by path, so it runs on every host.
//! `escalate_schedule` runs on a virtual clock, and each case sets when the first Ctrl+C returns.

#[path = "../src/window.rs"]
#[allow(dead_code)]
mod window;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use window::{Clock, Finish, MIN_INPUT_WAIT, escalate_schedule};

/// `INTERRUPT_WAIT` in `backend.rs`.
const WAIT: Duration = Duration::from_millis(500);
/// `POLL_INTERVAL` in `interrupt.rs`.
const POLL: Duration = Duration::from_millis(250);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[derive(Clone, Default)]
struct Virtual(Rc<Cell<Duration>>);

impl Virtual {
    fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by);
    }
}

impl Clock for Virtual {
    fn elapsed(&self) -> Duration {
        self.0.get()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// Runs the schedule against a job that never empties. The first Ctrl+C returns after
/// `first_return`, and every later Ctrl+C returns at once, not pending. Returns the window of
/// each Ctrl+C in the order it was written.
fn ctrl_c_windows(first_return: Duration) -> Vec<Duration> {
    let clock = Virtual::default();
    let windows = RefCell::new(Vec::new());
    let outcome = escalate_schedule(
        WAIT,
        POLL,
        &clock,
        || Ok(1),
        || Ok(()),
        |window| {
            let first = windows.borrow().is_empty();
            windows.borrow_mut().push(window);
            if first {
                clock.advance(first_return);
            }
            Ok(false)
        },
    );
    assert_eq!(outcome.ok(), Some(Finish::Terminated));
    windows.into_inner()
}

#[test]
fn a_first_ctrl_c_inside_the_band_skips_the_second() {
    // Wake at 490 ms leaves 10 ms, under the floor.
    assert_eq!(ctrl_c_windows(ms(240)), vec![WAIT]);
}

#[test]
fn a_first_ctrl_c_just_below_the_band_still_gets_the_second_at_the_floor() {
    // Wake at 480 ms leaves exactly MIN_INPUT_WAIT.
    assert_eq!(ctrl_c_windows(ms(230)), vec![WAIT, MIN_INPUT_WAIT]);
}

#[test]
fn a_first_ctrl_c_at_the_top_of_the_band_writes_the_second_at_once() {
    // At half of the wait the second is written with the 250 ms that remain.
    assert_eq!(ctrl_c_windows(ms(250)), vec![WAIT, ms(250)]);
}

#[test]
fn a_first_ctrl_c_inside_the_late_band_skips_the_second() {
    assert_eq!(ctrl_c_windows(ms(490)), vec![WAIT]);
}

#[test]
fn a_first_ctrl_c_at_the_start_of_the_late_band_is_not_skipped() {
    // Written at once with exactly MIN_INPUT_WAIT left.
    assert_eq!(ctrl_c_windows(ms(480)), vec![WAIT, MIN_INPUT_WAIT]);
}
