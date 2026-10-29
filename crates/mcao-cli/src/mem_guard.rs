//! `mcao mem-guard`: the host memory and swap verdict, read once through the shared host sampler.
//!
//! The verdict, its thresholds, and both renderers live in `mcao_core::resources`. This module
//! parses the flags, prints the report, and maps the verdict to an exit status. UNKNOWN exits 0
//! even under `--check`, because the verdict fails open.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Args;
use mcao_core::resources::{
    GuardLimits, GuardStatus, SysSampler, assess, render_json, render_text, utc_stamp,
};
use mcao_core::util::clock;

/// Flags for `mcao mem-guard`. Each threshold is a percentage, as the reference tool takes it.
#[derive(Debug, Args)]
pub struct MemGuardArgs {
    /// Memory use, in percent of total memory, that reads as a warning.
    #[arg(long, default_value_t = 90.0)]
    warn_mem_pct: f64,
    /// Memory use, in percent of total memory, that reads as critical.
    #[arg(long, default_value_t = 95.0)]
    crit_mem_pct: f64,
    /// Swap use, in percent of total swap, that reads as a warning.
    #[arg(long, default_value_t = 85.0)]
    warn_swap_pct: f64,
    /// Swap use, in percent of total swap, that reads as critical.
    #[arg(long, default_value_t = 95.0)]
    crit_swap_pct: f64,
    /// Print the report as JSON instead of text.
    #[arg(long)]
    json: bool,
    /// Exit 1 for a WARNING or CRITICAL verdict. UNKNOWN still exits 0.
    #[arg(long)]
    check: bool,
}

/// Runs `mcao mem-guard`: reads the host, prints the report, and returns the exit status.
pub fn run(args: &MemGuardArgs) -> ExitCode {
    let limits = GuardLimits {
        warn_mem_pct: args.warn_mem_pct,
        crit_mem_pct: args.crit_mem_pct,
        warn_swap_pct: args.warn_swap_pct,
        crit_swap_pct: args.crit_swap_pct,
    };
    let epoch_secs = u64::try_from(clock::now_ms() / 1000).unwrap_or_default();
    let report = assess(&SysSampler::new(), limits, utc_stamp(epoch_secs));
    let text = if args.json {
        render_json(&report)
    } else {
        render_text(&report)
    };
    if let Err(err) = writeln!(io::stdout().lock(), "{text}") {
        eprintln!("mcao mem-guard: cannot write the report: {err}");
        return ExitCode::from(1);
    }
    if fails(args.check, report.status) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Whether the verdict fails the process: only under `--check`, and only for WARNING or CRITICAL.
fn fails(check: bool, status: GuardStatus) -> bool {
    check && status.fails_check()
}

#[cfg(test)]
mod tests {
    use super::{GuardStatus, fails};

    #[test]
    fn check_fails_only_for_warning_and_critical() {
        assert!(fails(true, GuardStatus::Warning));
        assert!(fails(true, GuardStatus::Critical));
        assert!(!fails(true, GuardStatus::Ok));
        assert!(!fails(true, GuardStatus::Unknown));
    }

    #[test]
    fn without_check_nothing_fails() {
        assert!(!fails(false, GuardStatus::Critical));
    }
}
