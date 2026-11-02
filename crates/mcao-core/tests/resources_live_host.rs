//! Live macOS checks for the memory and swap figures (DPES-866 review attempt 1, F1; DPES-998 F5).
//!
//! The sampler's available memory must agree with a figure computed from `vm_stat` on the same
//! host, within a stated tolerance. `vm_stat` is an independent read of the kernel counters, so
//! this checks that `sysinfo` reads the right ones. The swap total must agree with
//! `sysctl -n vm.swapusage`. The file compiles to nothing off macOS, and it runs `vm_stat`, `top`,
//! and `sysctl` as read-only queries. It is host evidence, not a fake-sampler test.

#![cfg(target_os = "macos")]

use std::process::Command;
use std::time::Duration;

use mcao_core::resources::{ResourceSampler, SysSampler};

/// Largest allowed gap, in percentage points of total memory, between the sampler and the
/// `vm_stat` figure. The two reads are taken a moment apart, so memory can move between them.
const TOLERANCE_POINTS: f64 = 2.0;

/// The `vm_stat` output, as text.
fn vm_stat() -> String {
    let output = Command::new("vm_stat").output().expect("vm_stat runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The page count on the `vm_stat` line that starts with `label`.
fn pages_of(text: &str, label: &str) -> u64 {
    let line = text
        .lines()
        .find(|line| line.starts_with(label))
        .unwrap_or_else(|| panic!("vm_stat prints `{label}`"));
    line[label.len()..]
        .trim()
        .trim_end_matches('.')
        .parse()
        .unwrap_or_else(|_| panic!("vm_stat's `{label}` count is a number"))
}

/// The page size in bytes that `vm_stat` prints on its first line.
fn page_size_of(text: &str) -> f64 {
    text.lines()
        .next()
        .and_then(|line| line.split("page size of ").nth(1))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|number| number.parse().ok())
        .expect("vm_stat prints its page size")
}

/// Available memory in bytes from `vm_stat`: free and speculative pages, plus active and inactive
/// pages. `vm_stat` prints free pages without the speculative ones, so both are added back.
fn vm_stat_available_bytes(text: &str) -> f64 {
    let pages = pages_of(text, "Pages free:")
        + pages_of(text, "Pages speculative:")
        + pages_of(text, "Pages active:")
        + pages_of(text, "Pages inactive:");
    pages as f64 * page_size_of(text)
}

/// Swap total in bytes, and the half-unit tolerance of its two-decimal print, from the first
/// `total` field of `sysctl -n vm.swapusage`, such as `total = 2048.00M`. `None` when the field is
/// missing or its unit is unknown.
fn printed_swap_total(text: &str) -> Option<(f64, f64)> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let index = tokens.iter().position(|token| *token == "total")?;
    let printed = tokens.get(index + 2)?;
    let unit = match printed.chars().last()? {
        'K' => 1024.0,
        'M' => 1024.0 * 1024.0,
        'G' => 1024.0 * 1024.0 * 1024.0,
        'T' => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let value: f64 = printed
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse()
        .ok()?;
    Some((value * unit, 0.005 * unit))
}

#[test]
fn printed_swap_total_reads_each_unit() {
    let (bytes, tolerance) = printed_swap_total("total = 2.00G  used = 0.00M").expect("G parses");
    assert_eq!(bytes, 2.0 * 1024.0 * 1024.0 * 1024.0);
    assert_eq!(tolerance, 0.005 * 1024.0 * 1024.0 * 1024.0);
    assert_eq!(
        printed_swap_total("total = 0.00M"),
        Some((0.0, 0.005 * 1024.0 * 1024.0))
    );
    assert_eq!(printed_swap_total("used = 0.00M"), None);
}

/// Swap total must match `sysctl -n vm.swapusage`. A host with no swap skips with a message. A
/// zeroed struct therefore fails on any host that has swap.
#[test]
fn resources_live_swap_total_matches_sysctl_on_macos() {
    let output = Command::new("sysctl")
        .args(["-n", "vm.swapusage"])
        .output()
        .expect("sysctl runs");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (printed_total, tolerance) =
        printed_swap_total(&text).expect("vm.swapusage prints a total");
    if printed_total == 0.0 {
        eprintln!("skipped: this host has no swap (vm.swapusage total is 0)");
        return;
    }
    let swap = SysSampler::new().swap().expect("swap is readable");
    assert!(
        swap.total > 0,
        "a zeroed swap reading must not pass on a host with swap: {swap:?}"
    );
    assert!(
        (swap.total as f64 - printed_total).abs() <= tolerance,
        "swap total {} differs from `sysctl -n vm.swapusage` {text:?}",
        swap.total
    );
    assert!(swap.used <= swap.total, "{swap:?}");
}

#[test]
fn resources_live_available_matches_vm_stat_on_macos() {
    let sampler = SysSampler::new();
    for round in 0..3 {
        let memory = sampler.memory().expect("host memory is readable");
        let sampled = memory.available as f64 * 100.0 / memory.total as f64;
        let text = vm_stat();
        let expected = vm_stat_available_bytes(&text) * 100.0 / memory.total as f64;
        eprintln!(
            "round {round}: sampler available {sampled:.1}% of {} bytes; vm_stat free+speculative+active+inactive {expected:.1}%",
            memory.total
        );
        assert!(
            (sampled - expected).abs() <= TOLERANCE_POINTS,
            "sampler available {sampled:.1}% differs from vm_stat's {expected:.1}% by more than {TOLERANCE_POINTS} points"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Reference only: `top`'s used figure also counts reclaimable file cache, so it reads higher.
#[test]
fn resources_live_top_reference_is_printed_not_asserted() {
    let output = Command::new("top")
        .args(["-l", "1", "-n", "0"])
        .output()
        .expect("top runs");
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("PhysMem:"))
        .expect("top prints a PhysMem line");
    eprintln!("top reference (includes file cache): {line}");
}
