//! Host resource sampling over `sysinfo` (DPES-866, slice A).
//!
//! [`SysSampler`] only reads. It starts no process, runs no shell command, and uses no
//! `unsafe`. A value it cannot supply is an error, which [`super::sample`] reports as unknown
//! rather than zero.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use sysinfo::{
    CpuRefreshKind, Disk, DiskRefreshKind, Disks, MINIMUM_CPU_UPDATE_INTERVAL, MemoryRefreshKind,
    ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};

use super::{DiskStats, MemoryStats, ProcessRss, ResourceSampler, SampleError};

/// Reads host memory, CPU, disk, and process figures through `sysinfo`.
///
/// Each call takes a fresh reading and keeps no state. CPU usage waits for the window given to
/// [`ResourceSampler::cpu_used_pct`] because `sysinfo` derives it from two samples.
///
/// What each reading can and cannot tell you:
///
/// - Memory: `available` is the OS available-memory figure. On Windows `sysinfo` documents it as
///   best effort rather than an exact count.
/// - Swap: on macOS and Linux it is swap in use. On Windows it is commit charge above physical
///   RAM, not page-file use, so a high value means committed memory is near the commit limit.
///   When `sysinfo` cannot read swap it reports zero, and this sampler cannot tell that apart
///   from a host with no swap. Swap therefore never reads as unknown through this sampler.
/// - Disk: the filesystem is the mount point with the most path components that is a prefix of
///   the canonical path. `sysinfo` does not list every filesystem type (on Linux it omits
///   network shares and autofs unless optional features are enabled). The chosen mount's device
///   id must match the path's, so a path on an unlisted filesystem is an error, never the
///   parent's figures. Nested btrfs subvolumes that are not separate mounts report their own
///   device id, so they are reported as errors too.
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
            available: system.available_memory(),
            swap_total: system.total_swap(),
            swap_used: system.used_swap(),
        })
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
