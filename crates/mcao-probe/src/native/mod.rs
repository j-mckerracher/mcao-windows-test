//! The native backend for the platform the probe was built for, driving the stub harness.
//!
//! `mcao-backend-unix` is an empty skeleton and `mcao-backend-windows` has a Job Object and a
//! suspended spawn but no terminal, so these adapters live here until those crates grow a
//! `SessionBackend`. Each one reports what it can observe and answers `Unsupported` for the rest, so
//! a missing capability is never recorded as demonstrated.

use mcao_core::SessionBackend;

use crate::args::Scenario;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Returns the real backend for this platform.
pub fn backend(scenario: Scenario) -> Box<dyn SessionBackend> {
    #[cfg(unix)]
    {
        Box::new(unix::UnixPipeBackend::new(scenario))
    }
    #[cfg(windows)]
    {
        // With no terminal there is no Escape to ignore, so the scenario cannot change the run.
        let _ = scenario;
        Box::new(windows::WindowsJobBackend::new())
    }
}

/// Why `reconnect` is not demonstrated on either platform.
#[cfg(any(unix, windows))]
const RECONNECT_UNSUPPORTED: &str = "no durable worker identity exists yet: this session dies with the probe process, so a controller restart cannot reattach to it";

/// Bounded wait used by spawn and cleanup checks: `WAIT_POLLS` reads, `WAIT_INTERVAL` apart.
#[cfg(any(unix, windows))]
const WAIT_POLLS: u32 = 50;
#[cfg(any(unix, windows))]
const WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
