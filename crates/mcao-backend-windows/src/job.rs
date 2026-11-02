//! Windows Job Object: a kernel object that owns a process tree.
//!
//! A process assigned to a job, and every descendant it creates, stays in that
//! job. [`JobObject::terminate`] ends the whole tree at once, and
//! [`JobObject::active_processes`] counts its live members.
//!
//! A job is named in the `Local\` namespace, so a later session can reopen it with
//! [`JobObject::open`] and reach the processes an earlier one started. [`probe_named`]
//! reads such a job without changing it.
//!
//! Dropping a [`JobObject`] closes its handle and does not end its processes. The job
//! never sets `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the job itself is not killed by its
//! controller going away and can be reopened by name. That does not make a worker
//! survive. On Windows ConPTY a worker ends when its pseudoconsole closes, which happens
//! when the orchestrator exits (see the `Session` docs). Recovery reads the probes for the
//! real state. Cleanup of processes that are still running belongs to the recovery path,
//! not to drop.

use std::io;
use std::iter;
use std::mem;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::time::Duration;

use mcao_core::process_identity::JobProbe;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_LIMIT_PROCESS_TIME, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE};

/// The namespace prefix that keeps a job in the current session.
const LOCAL_PREFIX: &str = r"Local\";

/// An owned Windows Job Object.
///
/// Dropping the value closes the job handle. It does not terminate the job's
/// processes.
#[derive(Debug)]
pub struct JobObject {
    handle: OwnedHandle,
}

/// Optional resource limits that apply to each process in a job.
///
/// `None` leaves that limit unset, so the default sets no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// The most committed memory, in bytes, that each process may use.
    pub process_memory_bytes: Option<u64>,
    /// The most user-mode CPU time that each process may accrue.
    pub per_process_user_time: Option<Duration>,
}

impl JobObject {
    /// Creates an unnamed job with default security and no limits.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `CreateJobObjectW` when the job cannot be created.
    pub fn new() -> io::Result<Self> {
        // SAFETY: Both arguments are null, which CreateJobObjectW documents as
        // default security and an unnamed job. The call reads no other memory
        // from this process.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a non-null handle that CreateJobObjectW just returned.
        // No other owner exists, so OwnedHandle takes sole ownership and closes
        // it exactly once.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(Self { handle })
    }

    /// Creates a job named `name`, which other processes can open by that name.
    ///
    /// The name may begin with `Local\`. No other backslash is accepted.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `name` contains NUL, or a backslash outside a
    /// leading `Local\`. Returns `AlreadyExists` when a job with that name already
    /// exists, because `CreateJobObjectW` would then hand back a handle to that other
    /// job. Returns the OS error from `CreateJobObjectW` otherwise.
    pub fn named(name: &str) -> io::Result<Self> {
        check_name(name)?;
        let wide = wide_name(name);
        // SAFETY: The security attributes are null, which selects default security.
        // `wide` is nul-terminated and lives for the whole call.
        let raw = unsafe { CreateJobObjectW(ptr::null(), wide.as_ptr()) };
        // Capture the error state before any other call can overwrite it. On
        // success it reads ERROR_ALREADY_EXISTS when the name was already taken.
        let status = io::Error::last_os_error();
        if raw.is_null() {
            return Err(status);
        }
        // SAFETY: `raw` is a non-null handle that CreateJobObjectW just returned.
        // No other owner exists, so OwnedHandle takes sole ownership and closes
        // it exactly once, including on the refusal below.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        if status.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a job with this name already exists",
            ));
        }
        Ok(Self { handle })
    }

    /// Opens the existing job named `name` with the rights to query and terminate it.
    ///
    /// An opened job cannot take new limits. [`JobObject::set_limits`] needs a job
    /// created in this process.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a name that [`JobObject::named`] would refuse.
    /// Returns the OS error from `OpenJobObjectW` otherwise, including `NotFound`
    /// when no job has that name.
    pub fn open(name: &str) -> io::Result<Self> {
        let handle = open_named(name, JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE)?;
        Ok(Self { handle })
    }

    /// Wraps an open job handle, so a test can hold a job with reduced access rights.
    #[cfg(test)]
    pub(crate) fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Assigns `process` to this job. Its future children join the job too.
    ///
    /// The caller keeps ownership of the process handle. Assignment fails if the
    /// process already belongs to a job that does not permit nesting.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `AssignProcessToJobObject`.
    pub fn assign(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        // SAFETY: `self.handle` is a valid job handle owned by this value.
        // `process` is a BorrowedHandle, so it is valid for the duration of the
        // call.
        let ok = unsafe {
            AssignProcessToJobObject(self.handle.as_raw_handle(), process.as_raw_handle())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Applies `limits` to every process in this job. A call with no limit set
    /// succeeds and changes nothing.
    ///
    /// Only a job created in this process accepts limits, because the call needs the
    /// set-attributes right.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero limit or one the kernel cannot hold. Returns
    /// the OS error from `SetInformationJobObject` otherwise.
    pub fn set_limits(&self, limits: JobLimits) -> io::Result<()> {
        if limits == JobLimits::default() {
            return Ok(());
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if let Some(bytes) = limits.process_memory_bytes {
            let limit = usize::try_from(bytes)
                .ok()
                .filter(|&limit| limit > 0)
                .ok_or_else(|| invalid_limit("the memory limit must be a positive byte count"))?;
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            info.ProcessMemoryLimit = limit;
        }
        if let Some(time) = limits.per_process_user_time {
            // The kernel counts CPU time in 100-nanosecond intervals.
            let intervals = i64::try_from(time.as_nanos() / 100)
                .ok()
                .filter(|&intervals| intervals > 0)
                .ok_or_else(|| invalid_limit("the CPU time limit must be at least 100 ns"))?;
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
            info.BasicLimitInformation.PerProcessUserTimeLimit = intervals;
        }
        let size = u32::try_from(mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("extended limit information size fits in u32");
        // SAFETY: `info` is a live JOBOBJECT_EXTENDED_LIMIT_INFORMATION, and `size` is
        // its exact size, so the kernel reads no more than that. `self.handle` is a
        // valid job handle owned by this value.
        let ok = unsafe {
            SetInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&info).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Reads back the limits the kernel holds for this job.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a value the kernel reports but this type cannot
    /// represent. Returns the OS error from `QueryInformationJobObject` otherwise.
    pub fn applied_limits(&self) -> io::Result<JobLimits> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let size = u32::try_from(mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("extended limit information size fits in u32");
        // SAFETY: `info` is a live, writable JOBOBJECT_EXTENDED_LIMIT_INFORMATION, and
        // `size` is its exact size, so the kernel writes no more than that. The
        // return-length pointer is null, which the API permits.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                ptr::from_mut(&mut info).cast(),
                size,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = info.BasicLimitInformation.LimitFlags;
        let process_memory_bytes = if (flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY) == 0 {
            None
        } else {
            Some(
                u64::try_from(info.ProcessMemoryLimit)
                    .map_err(|_| invalid_kernel_value("the memory limit"))?,
            )
        };
        let per_process_user_time = if (flags & JOB_OBJECT_LIMIT_PROCESS_TIME) == 0 {
            None
        } else {
            let intervals = u64::try_from(info.BasicLimitInformation.PerProcessUserTimeLimit)
                .map_err(|_| invalid_kernel_value("the CPU time limit"))?;
            Some(Duration::from_nanos(intervals.saturating_mul(100)))
        };
        Ok(JobLimits {
            process_memory_bytes,
            per_process_user_time,
        })
    }

    /// Terminates every process in the job with `exit_code`.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `TerminateJobObject`.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self.handle` is a valid job handle owned by this value. The exit
        // code is a plain integer.
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle(), exit_code) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Returns how many processes are currently live in the job.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `QueryInformationJobObject`.
    pub fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let size = u32::try_from(mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .expect("accounting information size fits in u32");
        // SAFETY: `info` is a live, writable JOBOBJECT_BASIC_ACCOUNTING_INFORMATION
        // and `size` is its exact size, so the kernel writes no more than that.
        // The optional return-length pointer is null, which the API permits.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                ptr::from_mut(&mut info).cast(),
                size,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }
}

/// Reads the job named `name` without changing it, and reports what it found.
///
/// The probe opens the job with the query right only, so it cannot terminate the
/// job. A missing job is [`JobProbe::Absent`]. A job that exists but cannot be read,
/// or a name this module refuses, is [`JobProbe::Unreadable`].
pub fn probe_named(name: &str) -> JobProbe {
    let handle = match open_named(name, JOB_OBJECT_QUERY) {
        Ok(handle) => handle,
        Err(error) => return probe_open_failure(&error),
    };
    match (JobObject { handle }).active_processes() {
        Ok(count) => JobProbe::Members(count),
        Err(_) => JobProbe::Unreadable,
    }
}

/// Maps the failure to open a named job onto the probe result it means.
fn probe_open_failure(error: &io::Error) -> JobProbe {
    match error.raw_os_error() {
        Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => JobProbe::Absent,
        _ => JobProbe::Unreadable,
    }
}

/// Opens the job named `name` with `access` rights.
fn open_named(name: &str, access: u32) -> io::Result<OwnedHandle> {
    check_name(name)?;
    let wide = wide_name(name);
    // SAFETY: `wide` is nul-terminated and lives for the whole call. The inherit
    // flag is 0, so children do not inherit the handle.
    let raw = unsafe { OpenJobObjectW(access, 0, wide.as_ptr()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a non-null handle that OpenJobObjectW just returned. No other
    // owner exists, so OwnedHandle takes sole ownership and closes it exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Refuses a name with NUL, or with a backslash outside its leading `Local\`.
fn check_name(name: &str) -> io::Result<()> {
    let bare = name.strip_prefix(LOCAL_PREFIX).unwrap_or(name);
    if bare.contains(['\0', '\\']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            r"job name cannot contain NUL, or a backslash outside a leading Local\ prefix",
        ));
    }
    Ok(())
}

/// Encodes `name` as nul-terminated UTF-16 for the Win32 call.
fn wide_name(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(iter::once(0)).collect()
}

fn invalid_limit(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

fn invalid_kernel_value(what: &str) -> io::Error {
    io::Error::other(format!(
        "{what} reported by the kernel does not fit JobLimits"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job name unique to this test process, in the session namespace.
    fn unique_name(label: &str) -> String {
        format!(r"Local\mcao-test-{}-{label}", std::process::id())
    }

    #[test]
    fn a_named_job_opens_by_name_and_probes_as_empty() {
        let name = unique_name("open");
        let created = JobObject::named(&name).expect("the job is created");
        let opened = JobObject::open(&name).expect("the named job opens");
        assert_eq!(opened.active_processes().expect("the count reads"), 0);
        assert!(matches!(probe_named(&name), JobProbe::Members(0)));
        drop(created);
    }

    #[test]
    fn a_second_job_with_the_same_name_is_refused() {
        let name = unique_name("twice");
        let first = JobObject::named(&name).expect("the first job is created");
        let refused = matches!(
            JobObject::named(&name),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        );
        assert!(refused, "a duplicate name reports AlreadyExists");
        drop(first);
    }

    #[test]
    fn an_unknown_name_probes_as_absent() {
        let probe = probe_named(&unique_name("absent"));
        assert!(matches!(probe, JobProbe::Absent));
    }

    #[test]
    fn names_outside_the_local_prefix_are_refused() {
        for name in [r"Global\mcao-x", r"Local\a\b", "a\0b"] {
            let refused = matches!(
                JobObject::open(name),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput
            );
            assert!(refused, "{name:?} is refused as invalid input");
        }
    }

    #[test]
    fn limits_read_back_as_set() {
        let job = JobObject::named(&unique_name("limits")).expect("the job is created");
        let limits = JobLimits {
            process_memory_bytes: Some(64 * 1024 * 1024),
            per_process_user_time: Some(Duration::from_secs(5)),
        };
        job.set_limits(limits).expect("the limits apply");
        assert_eq!(job.applied_limits().expect("the limits read back"), limits);
    }

    #[test]
    fn no_limits_is_a_successful_no_op() {
        let job = JobObject::new().expect("an unnamed job is created");
        job.set_limits(JobLimits::default())
            .expect("setting no limit succeeds");
        assert_eq!(
            job.applied_limits().expect("the limits read back"),
            JobLimits::default()
        );
    }

    #[test]
    fn a_zero_memory_limit_is_refused() {
        let job = JobObject::new().expect("an unnamed job is created");
        let refused = matches!(
            job.set_limits(JobLimits {
                process_memory_bytes: Some(0),
                per_process_user_time: None,
            }),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        );
        assert!(refused, "a zero memory limit is refused");
    }
}
