//! Process parent and start-time parsing for Linux, macOS, and Windows.
//!
//! Provides pure parsers for Linux `/proc/<pid>/stat` and the `btime` line of
//! `/proc/stat`, timestamp conversion from Windows `FILETIME` and Linux clock ticks
//! to Unix milliseconds, PID reuse validation guards, and output formatting. The
//! [`query_process_info`] function reads a live process through each platform's own interface
//! and feeds those pure functions.

use std::error::Error;
use std::fmt;

/// Exit code indicating success.
pub const EXIT_OK: i32 = 0;

/// Exit code indicating that querying process information failed.
pub const EXIT_QUERY_FAILED: i32 = 2;

/// Exit code indicating that the process start time could not be verified with certainty.
pub const EXIT_UNCERTAIN: i32 = 5;

/// Number of 100-nanosecond intervals between 1601-01-01 and the Unix epoch 1970-01-01.
const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;

/// Parsed parent process ID and start time in clock ticks from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// The parent process ID (`ppid`, field 4).
    pub ppid: u32,
    /// The process start time in clock ticks (`starttime`, field 22).
    pub start_ticks: u64,
}

/// Resolved parent process ID and process creation time in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessInfo {
    /// The parent process ID.
    pub parent_pid: u32,
    /// The process creation time in milliseconds since the Unix epoch.
    pub creation_time_ms: u64,
}

/// Errors that can occur when parsing process statistics or computing timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcInfoError {
    /// The input string was malformed or missing expected fields.
    Malformed(&'static str),
    /// Clock ticks per second was specified as zero.
    ZeroClockTick,
    /// An arithmetic overflow occurred during timestamp computation.
    Overflow,
}

impl fmt::Display for ProcInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(reason) => write!(f, "malformed proc info: {reason}"),
            Self::ZeroClockTick => write!(f, "clock ticks per second cannot be zero"),
            Self::Overflow => write!(f, "time calculation overflowed"),
        }
    }
}

impl Error for ProcInfoError {}

/// Parses parent PID and start time in clock ticks from Linux `/proc/<pid>/stat` content.
///
/// Finds the last `)` in `text` to handle process names containing spaces and parentheses,
/// then extracts field 4 (`ppid`) at index 1 and field 22 (`starttime`) at index 19 of the
/// remainder.
///
/// # Errors
///
/// Returns [`ProcInfoError::Malformed`] if `)` is missing, fewer than 20 fields follow it,
/// or `ppid` or `starttime` cannot be parsed.
pub fn parse_proc_stat(text: &str) -> Result<ProcStat, ProcInfoError> {
    let last_paren = text
        .rfind(')')
        .ok_or(ProcInfoError::Malformed("missing closing parenthesis"))?;
    let remainder = &text[last_paren + 1..];
    let fields: Vec<&str> = remainder.split_ascii_whitespace().collect();
    if fields.len() < 20 {
        return Err(ProcInfoError::Malformed(
            "fewer than 20 fields after command name",
        ));
    }
    let ppid: u32 = fields[1]
        .parse()
        .map_err(|_| ProcInfoError::Malformed("invalid ppid"))?;
    let start_ticks: u64 = fields[19]
        .parse()
        .map_err(|_| ProcInfoError::Malformed("invalid starttime"))?;
    Ok(ProcStat { ppid, start_ticks })
}

/// Parses system boot time in Unix milliseconds from Linux `/proc/stat` content.
///
/// Looks for the first line starting with the word `btime`, parses the second word as
/// seconds since the Unix epoch, and converts it to milliseconds.
///
/// # Errors
///
/// Returns [`ProcInfoError::Malformed`] if no `btime` line exists or the second word is not a
/// valid number, or [`ProcInfoError::Overflow`] if converting seconds to milliseconds overflows.
pub fn parse_btime_ms(proc_stat_text: &str) -> Result<u64, ProcInfoError> {
    for line in proc_stat_text.lines() {
        let mut words = line.split_whitespace();
        if let Some("btime") = words.next() {
            let sec_str = words
                .next()
                .ok_or(ProcInfoError::Malformed("missing btime value"))?;
            let seconds: u64 = sec_str
                .parse()
                .map_err(|_| ProcInfoError::Malformed("invalid btime number"))?;
            return seconds.checked_mul(1000).ok_or(ProcInfoError::Overflow);
        }
    }
    Err(ProcInfoError::Malformed("btime line not found"))
}

/// Computes process start time in Unix milliseconds from boot time and start ticks.
///
/// Calculates `boot_time_ms + start_ticks * 1000 / clock_ticks_per_sec` using `u128` arithmetic
/// with truncating integer division.
///
/// # Errors
///
/// Returns [`ProcInfoError::ZeroClockTick`] if `clock_ticks_per_sec` is 0, or
/// [`ProcInfoError::Overflow`] if the result does not fit in `u64`.
pub fn start_time_ms(
    boot_time_ms: u64,
    start_ticks: u64,
    clock_ticks_per_sec: u64,
) -> Result<u64, ProcInfoError> {
    if clock_ticks_per_sec == 0 {
        return Err(ProcInfoError::ZeroClockTick);
    }
    let ticks_ms = (start_ticks as u128) * 1000 / (clock_ticks_per_sec as u128);
    let total = (boot_time_ms as u128) + ticks_ms;
    u64::try_from(total).map_err(|_| ProcInfoError::Overflow)
}

/// Converts a Windows `FILETIME` timestamp to Unix milliseconds.
///
/// Windows `FILETIME` counts 100-nanosecond intervals since 1601-01-01 UTC.
/// Returns `None` if `filetime` is earlier than the Unix epoch (1970-01-01 UTC).
pub fn filetime_to_unix_ms(filetime: u64) -> Option<u64> {
    filetime
        .checked_sub(UNIX_EPOCH_FILETIME)
        .map(|ticks| ticks / 10_000)
}

/// Verdict on whether an observed process start time matches an expected start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartGuard {
    /// The start time matched the expected time (or no expectation was provided).
    Match,
    /// The start time differed from expected by more than 1000 milliseconds.
    Mismatch {
        /// The absolute difference in milliseconds.
        diff_ms: u64,
    },
}

/// Checks whether an actual start time matches an expected start time within tolerance.
///
/// If `expected_ms` is `None`, returns [`StartGuard::Match`].
/// If the absolute difference exceeds 1000 ms, returns [`StartGuard::Mismatch`].
/// Otherwise returns [`StartGuard::Match`].
pub fn check_start_guard(actual_ms: u64, expected_ms: Option<u64>) -> StartGuard {
    match expected_ms {
        None => StartGuard::Match,
        Some(expected) => {
            let diff = actual_ms.abs_diff(expected);
            if diff > 1000 {
                StartGuard::Mismatch { diff_ms: diff }
            } else {
                StartGuard::Match
            }
        }
    }
}

/// Supported output formatting modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// JSON object representation.
    Json,
    /// Plain decimal text output.
    Text,
}

/// Parses an output format identifier string.
///
/// Returns `Some(OutputFormat::Json)` for `"json"`, `Some(OutputFormat::Text)` for `"text"`,
/// and `None` for any other string.
pub fn parse_output_format(text: &str) -> Option<OutputFormat> {
    match text {
        "json" => Some(OutputFormat::Json),
        "text" => Some(OutputFormat::Text),
        _ => None,
    }
}

/// Formats process parent information according to the requested [`OutputFormat`].
///
/// For [`OutputFormat::Json`], produces `{"parent_pid": P, "creation_time_ms": C}`.
/// For [`OutputFormat::Text`], produces decimal parent PID.
/// Neither format includes a trailing newline.
pub fn render(info: &ProcessInfo, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => {
            format!(
                "{{\"parent_pid\": {}, \"creation_time_ms\": {}}}",
                info.parent_pid, info.creation_time_ms
            )
        }
        OutputFormat::Text => format!("{}", info.parent_pid),
    }
}

/// Returns the process exit code corresponding to a [`StartGuard`] verdict.
///
/// Returns [`EXIT_OK`] (0) for [`StartGuard::Match`] and [`EXIT_UNCERTAIN`] (5) for
/// [`StartGuard::Mismatch`].
pub fn guard_exit_code(guard: StartGuard) -> i32 {
    match guard {
        StartGuard::Match => EXIT_OK,
        StartGuard::Mismatch { .. } => EXIT_UNCERTAIN,
    }
}

/// Clock ticks per second that `/proc` uses for process start times (`USER_HZ`). Linux fixes it at
/// 100 for the architectures this crate builds for, so no `sysconf` call is needed.
#[cfg(target_os = "linux")]
const LINUX_USER_HZ: u64 = 100;

/// Why [`query_process_info`] could not produce a [`ProcessInfo`].
#[derive(Debug)]
pub enum ProcQueryError {
    /// The operating system did not answer, for example because the PID does not exist or the
    /// caller may not inspect that process.
    Unavailable {
        /// The PID that was queried.
        pid: u32,
        /// The operating system's reason.
        reason: String,
    },
    /// The operating system answered with data this module cannot interpret.
    Parse(ProcInfoError),
}

impl fmt::Display for ProcQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { pid, reason } => write!(f, "cannot query process {pid}: {reason}"),
            Self::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl Error for ProcQueryError {}

/// Wraps an I/O failure from a platform query as [`ProcQueryError::Unavailable`].
#[cfg(any(target_os = "linux", windows))]
fn unavailable(pid: u32, error: &std::io::Error) -> ProcQueryError {
    ProcQueryError::Unavailable {
        pid,
        reason: error.to_string(),
    }
}

/// Reads the parent PID and creation time of process `pid` from Linux `/proc`.
///
/// Reads `/proc/<pid>/stat` for the parent and start ticks, and the `btime` line of `/proc/stat`
/// for the boot time that the start ticks count from.
///
/// # Errors
///
/// Returns [`ProcQueryError::Unavailable`] if the process does not exist or `/proc` cannot be
/// read, and [`ProcQueryError::Parse`] if the files do not parse.
#[cfg(target_os = "linux")]
pub fn query_process_info(pid: u32) -> Result<ProcessInfo, ProcQueryError> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| unavailable(pid, &error))?;
    let ProcStat { ppid, start_ticks } = parse_proc_stat(&stat).map_err(ProcQueryError::Parse)?;
    let system = std::fs::read_to_string("/proc/stat").map_err(|error| unavailable(pid, &error))?;
    let boot_ms = parse_btime_ms(&system).map_err(ProcQueryError::Parse)?;
    let creation_time_ms =
        start_time_ms(boot_ms, start_ticks, LINUX_USER_HZ).map_err(ProcQueryError::Parse)?;
    Ok(ProcessInfo {
        parent_pid: ppid,
        creation_time_ms,
    })
}

/// Reads the parent PID and creation time of process `pid` from Windows.
///
/// The parent comes from `NtQueryInformationProcess` with `ProcessBasicInformation`, and the
/// creation time from `GetProcessTimes`, converted from `FILETIME` by [`filetime_to_unix_ms`].
///
/// # Errors
///
/// Returns [`ProcQueryError::Unavailable`] if the process does not exist or the query is refused,
/// and [`ProcQueryError::Parse`] if the creation time precedes the Unix epoch.
#[cfg(windows)]
pub fn query_process_info(pid: u32) -> Result<ProcessInfo, ProcQueryError> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Closes a process handle when it leaves scope, on every return path.
    struct CloseOnDrop(HANDLE);

    impl Drop for CloseOnDrop {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful `OpenProcess` and is closed only here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    // SAFETY: `OpenProcess` takes no pointer arguments, and a null handle means the call failed.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(unavailable(pid, &std::io::Error::last_os_error()));
    }
    let _handle = CloseOnDrop(handle);

    let mut basic = PROCESS_BASIC_INFORMATION::default();
    let mut returned = 0u32;
    // SAFETY: `basic` is a `PROCESS_BASIC_INFORMATION`, and the length passed is its exact size.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessBasicInformation,
            (&mut basic as *mut PROCESS_BASIC_INFORMATION).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return Err(ProcQueryError::Unavailable {
            pid,
            reason: format!("NtQueryInformationProcess returned NTSTATUS {status:#010x}"),
        });
    }

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: every out-pointer refers to a live `FILETIME` that the call may write.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(unavailable(pid, &std::io::Error::last_os_error()));
    }

    let parent_pid = u32::try_from(basic.InheritedFromUniqueProcessId).map_err(|_| {
        ProcQueryError::Parse(ProcInfoError::Malformed("parent PID exceeds 32 bits"))
    })?;
    let filetime = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let creation_time_ms = filetime_to_unix_ms(filetime).ok_or(ProcQueryError::Parse(
        ProcInfoError::Malformed("creation time precedes the Unix epoch"),
    ))?;
    Ok(ProcessInfo {
        parent_pid,
        creation_time_ms,
    })
}

/// The `struct proc_bsdinfo` that `proc_pidinfo` fills in, from the macOS SDK `<sys/proc_info.h>`.
///
/// The layout must match the C struct, so fields this module does not read stay in place.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[repr(C)]
#[derive(Default)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; 16],
    pbi_name: [u8; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

/// Reads the parent PID and creation time of process `pid` from macOS.
///
/// Calls `proc_pidinfo` with the `PROC_PIDTBSDINFO` flavor, which returns the parent PID and a
/// start time with microsecond precision.
///
/// # Errors
///
/// Returns [`ProcQueryError::Unavailable`] if the process does not exist or the query is refused,
/// and [`ProcQueryError::Parse`] if the start time overflows.
#[cfg(target_os = "macos")]
pub fn query_process_info(pid: u32) -> Result<ProcessInfo, ProcQueryError> {
    use std::ffi::{c_int, c_void};

    /// The `PROC_PIDTBSDINFO` flavor of `proc_pidinfo` (`<sys/proc_info.h>`).
    const PROC_PIDTBSDINFO: c_int = 3;
    /// The byte size of `struct proc_bsdinfo`, which `proc_pidinfo` must fill exactly.
    const PROC_BSDINFO_SIZE: c_int = size_of::<ProcBsdInfo>() as c_int;

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    let raw_pid = c_int::try_from(pid).map_err(|_| ProcQueryError::Unavailable {
        pid,
        reason: "PID exceeds the range of a C int".to_owned(),
    })?;
    let mut info = ProcBsdInfo::default();
    // SAFETY: `info` is a `#[repr(C)]` struct of exactly `PROC_BSDINFO_SIZE` bytes, and
    // `proc_pidinfo` writes at most `buffersize` bytes into the buffer it is given.
    let written = unsafe {
        proc_pidinfo(
            raw_pid,
            PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut ProcBsdInfo).cast(),
            PROC_BSDINFO_SIZE,
        )
    };
    if written != PROC_BSDINFO_SIZE {
        return Err(ProcQueryError::Unavailable {
            pid,
            reason: format!(
                "proc_pidinfo returned {written} of {PROC_BSDINFO_SIZE} bytes: {}",
                std::io::Error::last_os_error()
            ),
        });
    }

    let creation_time_ms = info
        .pbi_start_tvsec
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add(info.pbi_start_tvusec / 1000))
        .ok_or(ProcQueryError::Parse(ProcInfoError::Overflow))?;
    Ok(ProcessInfo {
        parent_pid: info.pbi_ppid,
        creation_time_ms,
    })
}

/// Reports that process queries are not implemented on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn query_process_info(pid: u32) -> Result<ProcessInfo, ProcQueryError> {
    Err(ProcQueryError::Unavailable {
        pid,
        reason: "process queries are not implemented on this platform".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_query_process_info_own_process_has_plausible_start_time() {
        let info =
            query_process_info(std::process::id()).expect("the test process can query itself");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after the Unix epoch")
            .as_millis() as u64;
        assert!(info.creation_time_ms <= now_ms + 2_000);
        assert!(info.creation_time_ms + 86_400_000 >= now_ms);
    }

    #[test]
    fn test_query_process_info_missing_pid_is_unavailable() {
        // No platform has a live process with PID u32::MAX.
        assert!(matches!(
            query_process_info(u32::MAX),
            Err(ProcQueryError::Unavailable { pid: u32::MAX, .. })
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_proc_bsdinfo_layout_matches_sdk_header() {
        assert_eq!(size_of::<ProcBsdInfo>(), 136);
        assert_eq!(std::mem::offset_of!(ProcBsdInfo, pbi_ppid), 16);
        assert_eq!(std::mem::offset_of!(ProcBsdInfo, pbi_start_tvsec), 120);
    }

    #[test]
    fn test_parse_proc_stat_standard() {
        let stat =
            "1234 (bash) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000 1000000 200";
        assert_eq!(
            parse_proc_stat(stat),
            Ok(ProcStat {
                ppid: 99,
                start_ticks: 5000,
            })
        );
    }

    #[test]
    fn test_parse_proc_stat_comm_with_parentheses() {
        let stat = "1234 (my (weird) cmd) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000 1000000 200";
        assert_eq!(
            parse_proc_stat(stat),
            Ok(ProcStat {
                ppid: 99,
                start_ticks: 5000,
            })
        );
    }

    #[test]
    fn test_parse_proc_stat_comm_with_spaces() {
        let stat =
            "1234 (a b) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000 1000000 200";
        assert_eq!(
            parse_proc_stat(stat),
            Ok(ProcStat {
                ppid: 99,
                start_ticks: 5000,
            })
        );
    }

    #[test]
    fn test_parse_proc_stat_exactly_20_fields() {
        let stat = "1234 (bash) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000";
        assert_eq!(
            parse_proc_stat(stat),
            Ok(ProcStat {
                ppid: 99,
                start_ticks: 5000,
            })
        );
    }

    #[test]
    fn test_parse_proc_stat_fewer_than_20_fields() {
        let stat = "1234 (bash) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0";
        assert!(matches!(
            parse_proc_stat(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_no_closing_paren() {
        let stat = "1234 bash S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000";
        assert!(matches!(
            parse_proc_stat(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_negative_ppid() {
        let stat = "1234 (bash) S -1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000";
        assert!(matches!(
            parse_proc_stat(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_invalid_ppid_alpha() {
        let stat = "1234 (bash) S x 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000";
        assert!(matches!(
            parse_proc_stat(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_negative_starttime() {
        let stat = "1234 (bash) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 -5";
        assert!(matches!(
            parse_proc_stat(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_empty() {
        assert!(matches!(
            parse_proc_stat(""),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_proc_stat_extra_trailing_fields_ignored() {
        let stat = "1234 (bash) S 99 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000 1000000 200 999 888";
        assert_eq!(
            parse_proc_stat(stat),
            Ok(ProcStat {
                ppid: 99,
                start_ticks: 5000,
            })
        );
    }

    #[test]
    fn test_parse_btime_ms_realistic() {
        let stat = "\
cpu  2255 34 2290 22625563 6290 127 456 0 0 0
cpu0 1132 17 1141 11311884 3657 72 216 0 0 0
intr 11493054 23 11 0 0 0 0 0 0 1
ctxt 1990473
btime 1700000000
processes 20120
procs_running 1
procs_blocked 0
";
        assert_eq!(parse_btime_ms(stat), Ok(1_700_000_000_000));
    }

    #[test]
    fn test_parse_btime_ms_absent() {
        let stat = "\
cpu  2255 34 2290 22625563
ctxt 1990473
processes 20120
";
        assert!(matches!(
            parse_btime_ms(stat),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_btime_ms_invalid_number() {
        assert!(matches!(
            parse_btime_ms("btime abc"),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_btime_ms_btimex_ignored() {
        let stat = "btimex 5\nbtime 1700000000\n";
        assert_eq!(parse_btime_ms(stat), Ok(1_700_000_000_000));

        let stat_only_btimex = "btimex 5\n";
        assert!(matches!(
            parse_btime_ms(stat_only_btimex),
            Err(ProcInfoError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_btime_ms_overflow() {
        assert_eq!(
            parse_btime_ms("btime 18446744073709551615"),
            Err(ProcInfoError::Overflow)
        );
    }

    #[test]
    fn test_parse_btime_ms_first_line_wins() {
        let stat = "btime 1700000000\nbtime 1800000000\n";
        assert_eq!(parse_btime_ms(stat), Ok(1_700_000_000_000));
    }

    #[test]
    fn test_start_time_ms_standard() {
        assert_eq!(start_time_ms(1_000, 5000, 100), Ok(51_000));
    }

    #[test]
    fn test_start_time_ms_truncation() {
        assert_eq!(start_time_ms(0, 1, 300), Ok(3));
    }

    #[test]
    fn test_start_time_ms_zero_clock_tick() {
        assert_eq!(
            start_time_ms(1000, 5000, 0),
            Err(ProcInfoError::ZeroClockTick)
        );
    }

    #[test]
    fn test_start_time_ms_overflow() {
        assert_eq!(
            start_time_ms(u64::MAX, u64::MAX, 1),
            Err(ProcInfoError::Overflow)
        );
    }

    #[test]
    fn test_start_time_ms_max_boundary() {
        assert_eq!(start_time_ms(0, u64::MAX, 1000), Ok(u64::MAX));
    }

    #[test]
    fn test_filetime_to_unix_ms_epoch() {
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000), Some(0));
    }

    #[test]
    fn test_filetime_to_unix_ms_epoch_plus_ten_thousand() {
        assert_eq!(filetime_to_unix_ms(116_444_736_000_010_000), Some(1));
    }

    #[test]
    fn test_filetime_to_unix_ms_before_epoch() {
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000 - 1), None);
    }

    #[test]
    fn test_filetime_to_unix_ms_zero() {
        assert_eq!(filetime_to_unix_ms(0), None);
    }

    #[test]
    fn test_filetime_to_unix_ms_realistic() {
        assert_eq!(
            filetime_to_unix_ms(133_000_000_000_000_000),
            Some(1_655_526_400_000)
        );
    }

    #[test]
    fn test_check_start_guard_none() {
        assert_eq!(check_start_guard(5000, None), StartGuard::Match);
    }

    #[test]
    fn test_check_start_guard_equal() {
        assert_eq!(check_start_guard(5000, Some(5000)), StartGuard::Match);
    }

    #[test]
    fn test_check_start_guard_diff_1000() {
        assert_eq!(check_start_guard(6000, Some(5000)), StartGuard::Match);
        assert_eq!(check_start_guard(4000, Some(5000)), StartGuard::Match);
    }

    #[test]
    fn test_check_start_guard_diff_1001_mismatch() {
        // Actual above expected
        assert_eq!(
            check_start_guard(6001, Some(5000)),
            StartGuard::Mismatch { diff_ms: 1001 }
        );
        // Actual below expected
        assert_eq!(
            check_start_guard(3999, Some(5000)),
            StartGuard::Mismatch { diff_ms: 1001 }
        );
    }

    #[test]
    fn test_parse_output_format() {
        assert_eq!(parse_output_format("json"), Some(OutputFormat::Json));
        assert_eq!(parse_output_format("text"), Some(OutputFormat::Text));
        assert_eq!(parse_output_format("JSON"), None);
        assert_eq!(parse_output_format(""), None);
    }

    #[test]
    fn test_render() {
        let info = ProcessInfo {
            parent_pid: 99,
            creation_time_ms: 51000,
        };
        assert_eq!(
            render(&info, OutputFormat::Json),
            "{\"parent_pid\": 99, \"creation_time_ms\": 51000}"
        );
        assert_eq!(render(&info, OutputFormat::Text), "99");
    }

    #[test]
    fn test_exit_codes_and_constants() {
        assert_eq!(EXIT_OK, 0);
        assert_eq!(EXIT_QUERY_FAILED, 2);
        assert_eq!(EXIT_UNCERTAIN, 5);
        assert_eq!(guard_exit_code(StartGuard::Match), EXIT_OK);
        assert_eq!(
            guard_exit_code(StartGuard::Mismatch { diff_ms: 1001 }),
            EXIT_UNCERTAIN
        );
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            ProcInfoError::Malformed("bad data").to_string(),
            "malformed proc info: bad data"
        );
        assert_eq!(
            ProcInfoError::ZeroClockTick.to_string(),
            "clock ticks per second cannot be zero"
        );
        assert_eq!(
            ProcInfoError::Overflow.to_string(),
            "time calculation overflowed"
        );
    }
}
