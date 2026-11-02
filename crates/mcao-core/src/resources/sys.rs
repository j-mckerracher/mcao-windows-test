//! Host resource sampling over `sysinfo` (DPES-866, slice A).
//!
//! [`SysSampler`] only reads. It starts no process and runs no shell command. Its one `unsafe`
//! call is a read-only `sysctlbyname` on macOS. A value it cannot supply is an error, which
//! [`super::sample`] reports as unknown rather than zero.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use sysinfo::{
    CpuRefreshKind, Disk, DiskRefreshKind, Disks, MINIMUM_CPU_UPDATE_INTERVAL, MemoryRefreshKind,
    ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};

use super::{DiskStats, MemoryStats, ProcessRss, ResourceSampler, SampleError, SwapStats};

/// Reads host memory, CPU, disk, and process figures through `sysinfo`.
///
/// Each call takes a fresh reading and keeps no state. CPU usage waits for the window given to
/// [`ResourceSampler::cpu_used_pct`] because `sysinfo` derives it from two samples.
///
/// What each reading can and cannot tell you:
///
/// - Memory: `available` is `sysinfo`'s available figure. On Linux it is `MemAvailable`. On macOS
///   it is the active, inactive, and free pages `sysinfo` sums, which counts active application
///   memory as available, so it reads lower use than Activity Monitor's Memory Used. This is the
///   figure the sampler used before PR 351 (DPES-866). PR 351 replaced it on macOS with total minus
///   used, and DPES-998 restored it. The shell reference reads `MemAvailable` from `/proc/meminfo`
///   and has no macOS source. A failed macOS memory query, which `sysinfo` reports as zero
///   available memory, is an error, never 0% used. On Windows `sysinfo` documents available memory
///   as best effort rather than an exact count.
/// - Swap: on macOS and Linux it is swap in use. On Windows it is commit charge above physical
///   RAM, not page-file use, so a high value means committed memory is near the commit limit.
///   Swap is unknown, never zero, whenever this sampler cannot confirm it. On Linux that means
///   `/proc/meminfo` omits `SwapTotal` or `SwapFree`. On Windows a zero total cannot be told apart
///   from a failed query, so it is unknown. On macOS [`ResourceSampler::swap`] reads the
///   `vm.swapusage` sysctl, where a failed call is an error and a successful call with a zero
///   total is no swap. The mem-guard takes swap from `memory()` instead, on every platform. On
///   macOS `sysinfo` reads that same sysctl and reports 0 when it fails, so the mem-guard can show
///   0% swap in that case.
/// - Disk: the filesystem is the mount point with the most path components that is a prefix of
///   the canonical path. `sysinfo` does not list every filesystem type (on Linux it omits network
///   shares and autofs unless optional features are enabled). On Unix the mount's device id must
///   match the path's, so a path on an unlisted filesystem is an error, never the parent's figures.
///   Nested btrfs subvolumes that are not separate mounts report their own device id, so they are
///   reported as errors too. Windows has no device check, because volume serial numbers are not
///   stable enough to compare. A path on a volume `sysinfo` does not list therefore reads the
///   figures of the drive root that prefix-matches it. Native Windows behavior is unverified.
/// - Processes: resident memory only. A process whose memory `sysinfo` cannot read reports
///   zero bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct SysSampler;

impl SysSampler {
    /// Creates a sampler. It holds no state, so every sampler reads the same way.
    pub const fn new() -> Self {
        Self
    }
}

impl ResourceSampler for SysSampler {
    fn memory(&self) -> Result<MemoryStats, SampleError> {
        let refresh =
            RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram().with_swap());
        let system = System::new_with_specifics(refresh);
        let total = system.total_memory();
        if total == 0 {
            return Err(SampleError::new("host reported zero total memory"));
        }
        Ok(MemoryStats {
            total,
            available: available_of(&system, total)?,
            swap_total: system.total_swap(),
            swap_used: system.used_swap(),
        })
    }

    /// Swap counters. Zero swap is returned as zero only where it is a confirmed reading; see the
    /// type docs for each platform's rule.
    fn swap(&self) -> Result<SwapStats, SampleError> {
        read_swap(self)
    }

    fn cpu_used_pct(&self, window: Duration) -> Result<f64, SampleError> {
        let refresh = RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_cpu_usage());
        let mut system = System::new_with_specifics(refresh);
        thread::sleep(window.max(MINIMUM_CPU_UPDATE_INTERVAL));
        system.refresh_cpu_usage();
        if system.cpus().is_empty() {
            return Err(SampleError::new("host reported no CPUs"));
        }
        let usage = f64::from(system.global_cpu_usage());
        if !usage.is_finite() {
            return Err(SampleError::new("host CPU usage is not a finite number"));
        }
        Ok(usage.clamp(0.0, 100.0))
    }

    fn disk(&self, path: &Path) -> Result<DiskStats, SampleError> {
        let resolved = path
            .canonicalize()
            .map_err(|err| SampleError::new(format!("cannot resolve {}: {err}", path.display())))?;
        let target = mount_form(&resolved);
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        let mount_points: Vec<&Path> = disks.list().iter().map(Disk::mount_point).collect();
        let index = pick_mount(&target, &mount_points).ok_or_else(|| {
            SampleError::new(format!("no mounted filesystem contains {}", path.display()))
        })?;
        let disk = &disks.list()[index];
        check_same_device(&resolved, disk.mount_point())?;
        if disk.total_space() == 0 {
            return Err(SampleError::new(format!(
                "filesystem at {} reports zero capacity",
                disk.mount_point().display()
            )));
        }
        Ok(DiskStats {
            total: disk.total_space(),
            available: disk.available_space(),
        })
    }

    fn top_processes(&self, n: usize) -> Result<Vec<ProcessRss>, SampleError> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        let processes: Vec<ProcessRss> = system
            .processes()
            .iter()
            .map(|(pid, process)| ProcessRss {
                pid: pid.as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                rss_bytes: process.memory(),
            })
            .collect();
        if processes.is_empty() {
            return Err(SampleError::new("host listed no processes"));
        }
        Ok(top_n(processes, n))
    }
}

/// The memory the host can still use, in bytes: `sysinfo`'s available figure on every platform.
#[cfg(target_os = "macos")]
fn available_of(system: &System, total: u64) -> Result<u64, SampleError> {
    available_from_sysinfo(total, system.available_memory())
}

#[cfg(not(target_os = "macos"))]
fn available_of(system: &System, _total: u64) -> Result<u64, SampleError> {
    Ok(system.available_memory())
}

/// Available memory on macOS from `sysinfo`'s available figure, capped at total. Zero available
/// memory is how `sysinfo` reports a failed `host_statistics64` query, so it is an error here, not
/// a reading of 100% used.
#[cfg(any(target_os = "macos", test))]
fn available_from_sysinfo(total: u64, available: u64) -> Result<u64, SampleError> {
    if available == 0 {
        return Err(SampleError::new(
            "host reported zero available memory, which a failed memory query also reports",
        ));
    }
    Ok(available.min(total))
}

/// macOS swap, read from `vm.swapusage`. It does not depend on `memory()`, so a failed memory read
/// leaves only the memory reading unknown.
#[cfg(target_os = "macos")]
fn read_swap(_sampler: &SysSampler) -> Result<SwapStats, SampleError> {
    let usage = swap_usage_sysctl(c"vm.swapusage")?;
    Ok(SwapStats {
        total: usage.xsu_total,
        used: usage.xsu_used,
    })
}

/// Swap from `sysinfo`'s memory read, with the Linux and Windows checks for a zero that may be a
/// failed read.
#[cfg(not(target_os = "macos"))]
fn read_swap(sampler: &SysSampler) -> Result<SwapStats, SampleError> {
    let memory = sampler.memory()?;
    check_swap_source()?;
    if memory.swap_total == 0 && cfg!(windows) {
        return Err(SampleError::new(
            "swap total reads 0, which a failed Windows query also reports",
        ));
    }
    Ok(SwapStats {
        total: memory.swap_total,
        used: memory.swap_used,
    })
}

/// Reads a macOS swap-usage sysctl. A failed call is an error, never zero. The call only reads
/// kernel state and writes at most the size of the struct it is given.
#[cfg(target_os = "macos")]
fn swap_usage_sysctl(name: &std::ffi::CStr) -> Result<libc::xsw_usage, SampleError> {
    let mut usage = libc::xsw_usage {
        xsu_total: 0,
        xsu_avail: 0,
        xsu_used: 0,
        xsu_pagesize: 0,
        xsu_encrypted: 0,
    };
    let mut len = std::mem::size_of::<libc::xsw_usage>();
    // SAFETY: `name` is NUL-terminated. `usage` is a live, writable `xsw_usage`, and `len` holds its
    // size, so the kernel writes no more than that. No new value is passed in.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut usage).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(SampleError::new(format!(
            "sysctl {} failed: {}",
            name.to_string_lossy(),
            std::io::Error::last_os_error()
        )));
    }
    if len != std::mem::size_of::<libc::xsw_usage>() {
        return Err(SampleError::new(format!(
            "sysctl {} returned an unexpected size",
            name.to_string_lossy()
        )));
    }
    Ok(usage)
}

/// Confirms that `/proc/meminfo` lists both swap keys. `sysinfo` leaves swap at zero when either key
/// is missing, so a zero there is not evidence of no swap.
#[cfg(target_os = "linux")]
fn check_swap_source() -> Result<(), SampleError> {
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|err| SampleError::new(format!("cannot read /proc/meminfo: {err}")))?;
    for key in ["SwapTotal", "SwapFree"] {
        if !meminfo_lists(&text, key) {
            return Err(SampleError::new(format!(
                "{key} is missing from /proc/meminfo"
            )));
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn check_swap_source() -> Result<(), SampleError> {
    Ok(())
}

/// Whether `/proc/meminfo` text lists `key` as a whole `key:` field name.
#[cfg(any(target_os = "linux", test))]
fn meminfo_lists(text: &str, key: &str) -> bool {
    text.lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(name, _)| name.trim() == key)
    })
}

/// Chooses the mount point that holds `target` and returns its index in `mounts`.
///
/// A mount holds `target` when it is a whole-component prefix of it, so `/data` does not hold
/// `/database`. The mount with the most components wins, so a nested mount shadows its parent.
/// When two holding mounts have the same depth, the later entry wins, matching `/proc/mounts`
/// order, where a later mount at the same path covers an earlier one.
fn pick_mount<P: AsRef<Path>>(target: &Path, mounts: &[P]) -> Option<usize> {
    mounts
        .iter()
        .enumerate()
        .filter_map(|(index, mount)| {
            let mount = mount.as_ref();
            target
                .starts_with(mount)
                .then_some((index, mount.components().count()))
        })
        .max_by_key(|&(_, depth)| depth)
        .map(|(index, _)| index)
}

/// Rewrites a canonical path into the form `sysinfo` uses for mount points.
///
/// On Windows, `canonicalize` returns a verbatim path such as `\\?\C:\data`, while mount points
/// are plain drive roots such as `C:\`. The verbatim prefix is dropped only before a drive
/// letter. Verbatim UNC paths keep their prefix, because no drive mount point can match them.
/// Canonical Unix paths start with `/`, so they pass through unchanged.
fn mount_form(path: &Path) -> PathBuf {
    path.to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(
            |rest| matches!(rest.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic()),
        )
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Confirms that `path` lies on the filesystem mounted at `mount`, by comparing device ids.
///
/// The prefix match can pick a mount above the filesystem that really holds `path`, when
/// `sysinfo` leaves that filesystem out of its list. Equal device ids show the figures describe
/// `path` itself.
#[cfg(unix)]
fn check_same_device(path: &Path, mount: &Path) -> Result<(), SampleError> {
    use std::os::unix::fs::MetadataExt;

    let device_of = |location: &Path| {
        std::fs::metadata(location)
            .map(|meta| meta.dev())
            .map_err(|err| SampleError::new(format!("cannot stat {}: {err}", location.display())))
    };
    if device_of(path)? == device_of(mount)? {
        Ok(())
    } else {
        Err(SampleError::new(format!(
            "{} is not on the filesystem mounted at {}",
            path.display(),
            mount.display()
        )))
    }
}

/// Windows volume serial numbers are not stable enough to compare, so no check runs there.
#[cfg(not(unix))]
fn check_same_device(_path: &Path, _mount: &Path) -> Result<(), SampleError> {
    Ok(())
}

/// Orders processes by resident memory, largest first, with lower pid first on a tie, and keeps
/// the first `n`.
fn top_n(mut processes: Vec<ProcessRss>, n: usize) -> Vec<ProcessRss> {
    processes.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid)));
    processes.truncate(n);
    processes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_available_memory_is_unknown_not_full() {
        assert!(available_from_sysinfo(16, 0).is_err());
    }

    #[test]
    fn available_memory_is_the_sysinfo_figure_capped_at_total() {
        assert_eq!(available_from_sysinfo(16, 10), Ok(10));
        assert_eq!(available_from_sysinfo(16, 16), Ok(16));
        assert_eq!(available_from_sysinfo(16, 20), Ok(16));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_swap_sysctl_is_an_error_not_zero() {
        let err = swap_usage_sysctl(c"vm.mcao_no_such_sysctl").expect_err("unknown sysctl fails");
        assert!(err.reason.contains("vm.mcao_no_such_sysctl"), "{err}");
    }

    #[test]
    fn meminfo_lists_matches_whole_field_names_only() {
        let text = "MemTotal:       16 kB\nSwapTotal:      0 kB\nSwapTotalExtra: 5 kB\n";
        assert!(meminfo_lists(text, "SwapTotal"));
        assert!(!meminfo_lists(text, "SwapFree"));
        assert!(!meminfo_lists(text, "Swap"));
    }

    #[test]
    fn nested_mounts_choose_the_deepest_holding_mount() {
        let mounts = ["/", "/data", "/data/x"];
        assert_eq!(pick_mount(Path::new("/data/x/y"), &mounts), Some(2));
        assert_eq!(pick_mount(Path::new("/data/z"), &mounts), Some(1));
        assert_eq!(pick_mount(Path::new("/other"), &mounts), Some(0));
    }

    #[test]
    fn mount_must_match_whole_path_components() {
        let mounts = ["/", "/data"];
        assert_eq!(pick_mount(Path::new("/database/x"), &mounts), Some(0));
        assert_eq!(pick_mount(Path::new("/data"), &mounts), Some(1));
    }

    #[test]
    fn equal_depth_overmount_picks_the_later_entry() {
        let mounts = ["/", "/data", "/data"];
        assert_eq!(pick_mount(Path::new("/data/file"), &mounts), Some(2));
    }

    #[test]
    fn no_holding_mount_is_none() {
        let mounts = ["/data"];
        assert_eq!(pick_mount(Path::new("/other"), &mounts), None);
        let none: [&str; 0] = [];
        assert_eq!(pick_mount(Path::new("/data"), &none), None);
    }

    #[test]
    fn verbatim_drive_prefix_is_dropped() {
        assert_eq!(
            mount_form(Path::new(r"\\?\C:\data")),
            PathBuf::from(r"C:\data")
        );
    }

    #[test]
    fn verbatim_unc_prefix_is_kept() {
        let unc = Path::new(r"\\?\UNC\server\share");
        assert_eq!(mount_form(unc), unc.to_path_buf());
    }

    #[test]
    fn plain_path_is_unchanged() {
        assert_eq!(mount_form(Path::new("/data/x")), PathBuf::from("/data/x"));
    }

    fn process_with(pid: u32, rss_bytes: u64) -> ProcessRss {
        ProcessRss {
            pid,
            name: String::new(),
            rss_bytes,
        }
    }

    #[test]
    fn top_n_orders_by_rss_then_pid_and_keeps_n() {
        let input = vec![
            process_with(9, 0),
            process_with(4, 500),
            process_with(2, 0),
            process_with(7, 500),
            process_with(1, 0),
        ];
        let pids: Vec<u32> = top_n(input, 4).iter().map(|p| p.pid).collect();
        assert_eq!(pids, [4, 7, 1, 2]);
    }

    #[test]
    fn top_n_handles_zero_and_oversized_limits() {
        assert!(top_n(vec![process_with(1, 0)], 0).is_empty());
        assert_eq!(top_n(vec![process_with(1, 0)], 5).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn device_check_accepts_a_path_on_its_own_mount() {
        let here = std::env::temp_dir();
        assert!(check_same_device(&here, &here).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn device_check_rejects_a_path_on_another_filesystem() {
        use std::os::unix::fs::MetadataExt;

        let here = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir resolves");
        let other = Path::new("/dev");
        let device = |location: &Path| {
            std::fs::metadata(location)
                .expect("location is readable")
                .dev()
        };
        assert_ne!(
            device(&here),
            device(other),
            "/dev must sit on another filesystem than the temp dir for this test to mean anything"
        );
        assert!(
            check_same_device(&here, other).is_err(),
            "a path on another device must be rejected, never measured from the wrong mount"
        );
    }
}
