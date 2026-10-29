//! Suspended process creation that places the child in a job before it runs.
//!
//! `std::process::Command` cannot do this. It accepts `CREATE_SUSPENDED` through
//! `creation_flags`, but it returns no primary-thread handle and offers no hook
//! to attach the child to a job between creation and resume. So this module
//! calls `CreateProcessW` directly and resumes the primary thread only after
//! `AssignProcessToJobObject` succeeds. The child never runs outside the job, and
//! any grandchild it creates inherits the job.

use std::io;
use std::iter;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{self, Path, PathBuf};
use std::ptr;
use std::time::Duration;

use mcao_core::process_identity::{PidProbe, ProcessEvidence, ProcessIdentity, classify_process};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, GetProcessTimes, INFINITE, OpenProcess, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

use crate::conpty::{ConPty, ConsoleAttributes};
use crate::job::{self, JobObject};

/// `ResumeThread` reports failure as `(DWORD)-1`.
const RESUME_FAILED: u32 = u32::MAX;
/// The exit code that `GetExitCodeProcess` reports for a process still running,
/// `STILL_ACTIVE`.
const EXIT_STILL_ACTIVE: u32 = 259;
/// Exit code given to a suspended child that is killed after a failed setup step.
const SETUP_FAILED_EXIT: u32 = 1;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;

/// Whether a child starts a new console process group.
///
/// A new group makes the child's process id the group id that
/// `GenerateConsoleCtrlEvent` needs to send `CTRL_BREAK_EVENT` to the child and
/// its descendants alone. Windows also turns Ctrl+C off for the new group, so a
/// Ctrl+C written to the terminal no longer reaches the child. The two
/// interrupts cannot both work on one child, so the caller picks one at spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessGroup {
    /// The child stays in the creator's process group, and Ctrl+C reaches it.
    Inherit,
    /// The child leads a new process group, and `CTRL_BREAK_EVENT` can target it.
    New,
}

/// A child process that was created suspended and is already inside a job.
///
/// Dropping the value closes its handles. It does not terminate the child.
#[derive(Debug)]
pub struct ChildProcess {
    process: OwnedHandle,
    pid: u32,
}

impl ChildProcess {
    /// Returns the child's process id.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Waits up to `timeout` for the child to exit.
    ///
    /// Returns the exit code, or `None` if the child is still running when the
    /// wait expires. The wait is always bounded: a timeout that is too long for
    /// the OS wait is refused rather than treated as infinite.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `timeout` is at least `u32::MAX` milliseconds.
    /// Otherwise returns the OS error from `WaitForSingleObject` or
    /// `GetExitCodeProcess`.
    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<u32>> {
        let millis = u32::try_from(timeout.as_millis())
            .ok()
            .filter(|&ms| ms != INFINITE)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "timeout is too long for a bounded OS wait",
                )
            })?;
        // SAFETY: `self.process` is a live process handle owned by this value.
        // The wait is bounded by `millis`.
        let status = unsafe { WaitForSingleObject(self.process.as_raw_handle(), millis) };
        match status {
            WAIT_OBJECT_0 => {
                let mut code: u32 = 0;
                // SAFETY: The process handle is still live, and `code` is a valid
                // out pointer for the duration of the call.
                let ok = unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other(
                "WaitForSingleObject returned an unexpected status",
            )),
        }
    }

    /// Returns the child's creation time as a FILETIME value, which counts
    /// 100-nanosecond intervals since 1601-01-01 UTC.
    ///
    /// The time is read from the child's own handle, so it names this child and not
    /// the process that spawned it.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `GetProcessTimes`.
    pub fn creation_time(&self) -> io::Result<u64> {
        creation_time_of(self.process.as_raw_handle())
    }

    /// Returns the identity a later probe compares against: this child's process id
    /// and creation time.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `GetProcessTimes`.
    pub fn identity(&self) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity {
            pid: self.pid,
            start: self.creation_time()?,
        })
    }
}

/// Reads the creation time of the process behind `process`, which must carry the
/// query-limited-information right.
fn creation_time_of(process: HANDLE) -> io::Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live process handle owned by the caller. Each out
    // pointer is a valid, writable FILETIME for the duration of the call.
    let ok = unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// Probes the process with id `pid` and reports what it found, without changing it.
///
/// A process id that names no process is [`PidProbe::Absent`]. A process that
/// exists but cannot be opened or timed is [`PidProbe::Unreadable`]. A process that
/// has exited while a handle to it stays open reports `running: false`.
pub fn probe_pid(pid: u32) -> PidProbe {
    // SAFETY: OpenProcess takes plain values and returns a handle or null. A
    // non-null result is wrapped below, so it is closed exactly once.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return match io::Error::last_os_error().raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => PidProbe::Absent,
            _ => PidProbe::Unreadable,
        };
    }
    // SAFETY: `raw` is a non-null handle that OpenProcess just returned. No other
    // owner exists, so OwnedHandle takes sole ownership and closes it once.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let start = match creation_time_of(process.as_raw_handle()) {
        Ok(start) => start,
        Err(_) => return PidProbe::Unreadable,
    };
    let mut code: u32 = 0;
    // SAFETY: `process` is live, and `code` is a valid out pointer for the call.
    let ok = unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
    if ok == 0 {
        return PidProbe::Unreadable;
    }
    PidProbe::Found {
        start,
        running: code == EXIT_STILL_ACTIVE,
    }
}

/// Classifies the agent behind `stored` from its process and its named job.
///
/// Neither the process nor the job is changed. `job_name` is the name the agent was
/// started under, as `job::job_object_name` builds it.
pub fn agent_evidence(stored: ProcessIdentity, job_name: &str) -> ProcessEvidence {
    classify_process(stored, probe_pid(stored.pid), job::probe_named(job_name))
}

/// Starts `program` suspended, assigns it to `job`, and then resumes it.
///
/// `args` are passed after the program name using MSVC argument quoting. The
/// child inherits the current environment and working directory. Handle
/// inheritance is off and no standard handles are redirected, so stdio wiring is
/// left to a later slice.
///
/// The program must be an absolute path that is not a `.bat` or `.cmd` script.
/// `CreateProcessW` runs those through `cmd.exe`, where MSVC quoting does not
/// protect arguments. The path is normalized before that test, and the child is
/// started from the normalized path. Trailing dots and trailing spaces do not hide
/// a script. A program whose file name contains a colon is refused, because the
/// colon starts an NTFS stream designator such as `::$DATA`, and launching a
/// stream has no use here. Neither the program nor any argument may contain NUL,
/// because `CreateProcessW` would read the command line as ending there.
///
/// If assignment or resume fails, the suspended child is terminated before the
/// error is returned. That termination is best effort: if it fails, the child
/// stays suspended and is never resumed.
///
/// # Errors
///
/// Returns `InvalidInput` when the program or an argument breaks the rules above.
/// Otherwise returns the OS error from path normalization, `CreateProcessW`,
/// `AssignProcessToJobObject`, or `ResumeThread`. A missing program is reported
/// by `CreateProcessW`.
pub fn spawn_in_job(job: &JobObject, program: &Path, args: &[&str]) -> io::Result<ChildProcess> {
    create_suspended(program, args, None, ProcessGroup::Inherit)?
        .assign_to(job)?
        .resume()
}

/// Like [`spawn_in_job`], but attaches the child to `console` as its terminal and
/// starts it in `group`.
///
/// The child gets no inherited standard handles, so its console is the
/// pseudoconsole, not the parent's redirected stdio. All other rules and errors
/// of [`spawn_in_job`] apply.
///
/// # Errors
///
/// Returns the errors of [`spawn_in_job`], and the OS error from building the
/// pseudoconsole attribute list.
pub(crate) fn spawn_in_job_with_console(
    job: &JobObject,
    program: &Path,
    args: &[&str],
    console: &ConPty,
    group: ProcessGroup,
) -> io::Result<ChildProcess> {
    let mut attributes = console.attributes()?;
    create_suspended(program, args, Some(&mut attributes), group)?
        .assign_to(job)?
        .resume()
}

/// A child created suspended, before it belongs to any job.
///
/// Its only exit is [`SuspendedChild::assign_to`], which returns the
/// [`AssignedChild`]. Only that type has `resume`, so the child cannot run before
/// it is in the job. The ordering holds by construction.
struct SuspendedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

/// A suspended child that is inside its job and may now be resumed.
struct AssignedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

impl SuspendedChild {
    /// Assigns the suspended child to `job`. On failure the child is terminated.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `AssignProcessToJobObject`.
    fn assign_to(self, job: &JobObject) -> io::Result<AssignedChild> {
        if let Err(err) = job.assign(self.process.as_handle()) {
            terminate_suspended(&self.process);
            return Err(err);
        }
        Ok(AssignedChild {
            process: self.process,
            thread: self.thread,
            pid: self.pid,
        })
    }
}

impl AssignedChild {
    /// Resumes the primary thread. On failure the child is terminated.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `ResumeThread`.
    fn resume(self) -> io::Result<ChildProcess> {
        // SAFETY: `thread` is the live primary-thread handle that we own.
        let resumed = unsafe { ResumeThread(self.thread.as_raw_handle()) };
        if resumed == RESUME_FAILED {
            let err = io::Error::last_os_error();
            terminate_suspended(&self.process);
            return Err(err);
        }
        Ok(ChildProcess {
            process: self.process,
            pid: self.pid,
        })
    }
}

/// Creates `program` suspended, with no job assignment.
///
/// With `console` set, the child attaches to that pseudoconsole. Its standard
/// handles are then set to `INVALID_HANDLE_VALUE`, so it cannot inherit redirected
/// parent handles in place of the pseudoconsole. With `group` set to
/// [`ProcessGroup::New`], the child leads a new console process group.
fn create_suspended(
    program: &Path,
    args: &[&str],
    console: Option<&mut ConsoleAttributes<'_>>,
    group: ProcessGroup,
) -> io::Result<SuspendedChild> {
    let mut command_line = checked_command_line(program, args)?;
    // Every field not set here is zero, which requests no standard handles.
    let mut startup = STARTUPINFOEXW::default();
    let mut flags = CREATE_SUSPENDED;
    if group == ProcessGroup::New {
        flags |= CREATE_NEW_PROCESS_GROUP;
    }
    let startup_size = match console {
        Some(attributes) => {
            startup.lpAttributeList = attributes.as_mut_ptr();
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            flags |= EXTENDED_STARTUPINFO_PRESENT;
            mem::size_of::<STARTUPINFOEXW>()
        }
        None => mem::size_of::<STARTUPINFOW>(),
    };
    startup.StartupInfo.cb = u32::try_from(startup_size).expect("startup info size fits in u32");
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: `lpApplicationName` is null, so the command line names the program.
    // `command_line` is a mutable, nul-terminated buffer that outlives the call,
    // and CreateProcessW may write to it. The null pointers select inherited
    // security, environment, and working directory. `bInheritHandles` is FALSE.
    // `startup` and `info` are valid for the call. `cb` matches the structure that
    // `flags` announces, and the attribute list, when set, outlives the call
    // through the caller's borrow.
    let created = unsafe {
        CreateProcessW(
            ptr::null(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            flags,
            ptr::null(),
            ptr::null(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW succeeded, so `hProcess` is a new handle that nothing
    // else owns. Wrapping it transfers ownership, so it closes on drop.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    // SAFETY: CreateProcessW succeeded, so `hThread` is a new handle that nothing
    // else owns. Wrapping it transfers ownership, so it closes on drop.
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
    Ok(SuspendedChild {
        process,
        thread,
        pid: info.dwProcessId,
    })
}

/// Kills a child on a failure path where the original error is the one to report.
///
/// The result is ignored, because a termination failure cannot be reported without
/// hiding the error the caller is returning. The child then stays suspended.
fn terminate_suspended(process: &OwnedHandle) {
    // SAFETY: `process` is a live process handle borrowed from the caller. The
    // exit code is a plain integer.
    unsafe { TerminateProcess(process.as_raw_handle(), SETUP_FAILED_EXIT) };
}

/// Validates `program` and `args`, then builds the command line from the validated
/// path. This is the only way the launch path obtains a command line, so the path
/// that is checked is the path that runs.
fn checked_command_line(program: &Path, args: &[&str]) -> io::Result<Vec<u16>> {
    let program = validate(program, args)?;
    Ok(command_line(&program, args))
}

/// Refuses inputs that `CreateProcessW` would misread or route through `cmd.exe`.
///
/// Returns the normalized program path. The batch check reads that path, and the
/// caller must launch that same path, so what is checked is what runs.
fn validate(program: &Path, args: &[&str]) -> io::Result<PathBuf> {
    if !program.is_absolute() {
        return Err(invalid_input("program path must be absolute"));
    }
    let program_has_nul = program.as_os_str().encode_wide().any(|unit| unit == 0);
    if program_has_nul || args.iter().any(|arg| arg.contains('\0')) {
        return Err(invalid_input(
            "NUL is not allowed in the program path or in arguments",
        ));
    }
    // `absolute` calls `GetFullPathNameW`, the normalization Win32 applies to the
    // path before it opens the file. It rejects NUL, so the NUL check comes first.
    let normalized = path::absolute(program)?;
    // Both spellings are tested, so normalization can only add refusals.
    if has_stream_designator(program) || has_stream_designator(&normalized) {
        return Err(invalid_input(
            "program names with a colon are refused because they designate an NTFS stream",
        ));
    }
    if has_batch_extension(program) || has_batch_extension(&normalized) {
        return Err(invalid_input(
            "batch and cmd scripts are refused because cmd.exe would run them",
        ));
    }
    Ok(normalized)
}

/// Reports whether the final component of `program` contains a colon.
///
/// A colon cannot occur in a file name, so in the final component it can only start
/// an NTFS stream designator, as in `run.bat::$DATA` or `probe.exe:x.bat`. The
/// guard refuses every such name instead of working out which stream a name
/// reaches, so no stream spelling can hide a script. The drive colon belongs to
/// the path prefix and is not part of the final component.
fn has_stream_designator(program: &Path) -> bool {
    program
        .file_name()
        .is_some_and(|name| name.to_string_lossy().contains(':'))
}

/// Reports whether `program` names a `.bat` or `.cmd` script, in any letter case.
///
/// Win32 path resolution drops trailing dots and spaces from the final name
/// component, so `run.bat.` and `run.cmd ` name the same scripts as `run.bat` and
/// `run.cmd`. The check removes them before it reads the extension. Names with a
/// colon never reach this check, because [`has_stream_designator`] refuses them.
fn has_batch_extension(program: &Path) -> bool {
    let Some(name) = program.file_name() else {
        return false;
    };
    let name = name
        .to_string_lossy()
        .trim_end_matches(['.', ' '])
        .to_ascii_lowercase();
    name.ends_with(".bat") || name.ends_with(".cmd")
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Builds a nul-terminated UTF-16 command line for `program` and `args`.
fn command_line(program: &Path, args: &[&str]) -> Vec<u16> {
    let program_wide: Vec<u16> = program.as_os_str().encode_wide().collect();
    let mut line = Vec::new();
    push_arg(&mut line, &program_wide);
    for arg in args {
        line.push(SPACE);
        let wide: Vec<u16> = arg.encode_utf16().collect();
        push_arg(&mut line, &wide);
    }
    line.push(0);
    line
}

/// Appends one argument using the MSVC C runtime quoting rules.
///
/// An argument is wrapped in quotes when it is empty or contains a space, a tab,
/// or a quote. Inside quotes, a run of backslashes before a quote is doubled and
/// the quote is escaped. Trailing backslashes in a quoted argument are doubled so
/// the closing quote is not escaped.
fn push_arg(line: &mut Vec<u16>, arg: &[u16]) {
    let quoted = arg.is_empty() || arg.iter().any(|c| [SPACE, TAB, QUOTE].contains(c));
    if quoted {
        line.push(QUOTE);
    }
    // Backslashes are counted and written only once the next character is known,
    // because their output depends on what follows them.
    let mut pending = 0_usize;
    for &c in arg {
        if c == BACKSLASH {
            pending += 1;
            continue;
        }
        if c == QUOTE {
            line.extend(iter::repeat_n(BACKSLASH, pending * 2 + 1));
        } else {
            line.extend(iter::repeat_n(BACKSLASH, pending));
        }
        pending = 0;
        line.push(c);
    }
    if quoted {
        line.extend(iter::repeat_n(BACKSLASH, pending * 2));
        line.push(QUOTE);
    } else {
        line.extend(iter::repeat_n(BACKSLASH, pending));
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::System::JobObjects::OpenJobObjectW;
    use windows_sys::Win32::System::Threading::SuspendThread;

    use super::*;

    /// Quotes one argument and returns the result as a `String`.
    fn quoted(arg: &str) -> String {
        let wide: Vec<u16> = arg.encode_utf16().collect();
        let mut line = Vec::new();
        push_arg(&mut line, &wide);
        String::from_utf16(&line).expect("test input is valid UTF-16")
    }

    #[test]
    fn plain_argument_is_unchanged() {
        assert_eq!(quoted("plain"), "plain");
    }

    #[test]
    fn empty_argument_is_quoted() {
        assert_eq!(quoted(""), "\"\"");
    }

    #[test]
    fn argument_with_space_is_quoted() {
        assert_eq!(quoted("a b"), "\"a b\"");
    }

    #[test]
    fn embedded_quote_is_escaped() {
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn backslash_before_quote_is_doubled() {
        // Input is a, backslash, quote, b. Output is the quoted form of the same
        // text, with three backslashes before the escaped quote.
        assert_eq!(quoted("a\\\"b"), "\"a\\\\\\\"b\"");
    }

    #[test]
    fn trailing_backslash_unquoted_is_literal() {
        assert_eq!(quoted("a\\"), "a\\");
    }

    #[test]
    fn trailing_backslash_in_quoted_argument_is_doubled() {
        assert_eq!(quoted("a b\\"), "\"a b\\\\\"");
    }

    #[test]
    fn tab_argument_is_quoted() {
        assert_eq!(quoted("a\tb"), "\"a\tb\"");
    }

    /// Returns the kind of error `validate` reports for this program and arguments.
    fn refusal_kind(program: &str, args: &[&str]) -> io::ErrorKind {
        validate(Path::new(program), args)
            .expect_err("input is refused")
            .kind()
    }

    #[test]
    fn nul_in_argument_is_refused() {
        assert_eq!(
            refusal_kind(r"C:\tools\probe.exe", &["a\0b"]),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn nul_in_program_is_refused() {
        assert_eq!(
            refusal_kind("C:\\tools\\pro\0be.exe", &[]),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn batch_and_cmd_programs_are_refused_in_any_case() {
        for program in [
            r"C:\tools\run.bat",
            r"C:\tools\run.CMD",
            r"C:\tools\Run.Bat",
        ] {
            assert_eq!(
                refusal_kind(program, &[]),
                io::ErrorKind::InvalidInput,
                "{program}"
            );
        }
    }

    #[test]
    fn batch_and_cmd_programs_are_refused_after_trailing_dots_and_spaces_are_removed() {
        // Win32 resolves each of these to the script named without the trailing
        // characters, so the normalized extension decides the refusal.
        for program in [
            r"C:\tools\run.bat.",
            r"C:\tools\run.cmd ",
            r"C:\tools\run.Bat. .",
            r"C:\tools\run.CMD  ",
        ] {
            assert_eq!(
                refusal_kind(program, &[]),
                io::ErrorKind::InvalidInput,
                "{program}"
            );
        }
    }

    #[test]
    fn batch_and_cmd_programs_are_refused_with_an_ntfs_stream_suffix() {
        // `run.bat::$DATA` is the default data stream of `run.bat`, so it runs the
        // same script. A named stream and trailing characters before the colon
        // are refused as well, and so is a stream whose own name ends in `.bat`.
        for program in [
            r"C:\tools\run.bat::$DATA",
            r"C:\tools\run.CMD::$DATA",
            r"C:\tools\run.bat. ::$DATA",
            r"C:\tools\run.cmd :stream:$DATA",
            r"C:\tools\run.bat:stream",
            r"C:\tools\probe.exe:x.bat",
            r"C:\tools\probe.exe:x.cmd",
            r"C:\tools\probe.exe:x.BAT. ",
        ] {
            assert_eq!(
                refusal_kind(program, &[]),
                io::ErrorKind::InvalidInput,
                "{program}"
            );
        }
    }

    #[test]
    fn any_colon_in_the_program_name_is_refused() {
        // Launching an NTFS stream has no use here, so no stream spelling is
        // accepted, even one that does not resemble a script.
        for program in [
            r"C:\tools\probe.exe::$DATA",
            r"C:\tools\probe.exe:stream",
            r"C:\tools\run.bat.exe:stream",
        ] {
            assert_eq!(
                refusal_kind(program, &[]),
                io::ErrorKind::InvalidInput,
                "{program}"
            );
        }
    }

    #[test]
    fn validate_returns_the_normalized_path() {
        // `.` and `..` segments differ between the raw and the normalized path.
        for (raw, normalized) in [
            (r"C:\tools\.\probe.exe", r"C:\tools\probe.exe"),
            (r"C:\tools\sub\..\probe.exe", r"C:\tools\probe.exe"),
        ] {
            let validated = validate(Path::new(raw), &[]).expect("plain program is accepted");
            assert_eq!(validated, Path::new(normalized), "{raw}");
        }
    }

    #[test]
    fn launched_command_line_is_built_from_the_normalized_path() {
        let line = checked_command_line(Path::new(r"C:\tools\.\probe.exe"), &["a b"])
            .expect("plain program is accepted");
        let text = String::from_utf16(&line[..line.len() - 1]).expect("valid UTF-16");
        assert_eq!(text, r#"C:\tools\probe.exe "a b""#);
    }

    #[test]
    fn batch_program_is_refused_before_a_command_line_exists() {
        assert!(checked_command_line(Path::new(r"C:\tools\run.bat."), &[]).is_err());
    }

    #[test]
    fn names_that_only_resemble_batch_extensions_are_accepted() {
        for program in [
            r"C:\tools\run.batch",
            r"C:\tools\run.bat.exe",
            r"C:\tools\run.cmdx",
            r"C:\tools.cmd\probe.exe",
        ] {
            validate(Path::new(program), &[]).unwrap_or_else(|err| {
                panic!("{program} is not a batch script, but was refused: {err}")
            });
        }
    }

    #[test]
    fn relative_program_is_refused() {
        assert_eq!(refusal_kind("probe.exe", &[]), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn absolute_program_with_spaced_argument_is_accepted() {
        validate(Path::new(r"C:\tools\probe.exe"), &["--exact", "a b"])
            .expect("valid input is accepted");
    }

    #[test]
    fn command_line_quotes_program_and_separates_args() {
        let line = command_line(Path::new(r"C:\tools\mcao probe.exe"), &["--exact", "a b"]);
        let text = String::from_utf16(&line[..line.len() - 1]).expect("valid UTF-16");
        assert_eq!(text, r#""C:\tools\mcao probe.exe" --exact "a b""#);
        assert_eq!(line.last(), Some(&0));
    }

    /// Arguments that make the test binary exit 0 at once if it ever runs. A child
    /// that was terminated while suspended exits `SETUP_FAILED_EXIT` instead, so
    /// the exit code shows whether the child's code ran.
    const EXIT_AT_ONCE: [&str; 2] = ["--exact", "mcao-no-such-test"];
    /// Exit code of a child whose code ran to its normal end.
    const RAN_TO_END: u32 = 0;
    /// `JOB_OBJECT_QUERY` access right from the Windows SDK. A handle with only
    /// this right cannot assign a process to the job.
    const JOB_OBJECT_QUERY: u32 = 0x0004;
    /// Long enough for a terminated process to signal, short enough to end a hang.
    const EXIT_WAIT: Duration = Duration::from_secs(30);

    /// Creates the test binary suspended. The second value is a handle to the same
    /// process that stays valid after the child value is consumed.
    fn suspended_child() -> (SuspendedChild, ChildProcess) {
        let exe = env::current_exe().expect("test binary path is available");
        let child = create_suspended(&exe, &EXIT_AT_ONCE, None, ProcessGroup::Inherit)
            .expect("test binary starts suspended");
        let watcher = ChildProcess {
            process: child
                .process
                .try_clone()
                .expect("process handle duplicates"),
            pid: child.pid,
        };
        (child, watcher)
    }

    /// Waits for the child to exit and returns its exit code. A child still alive
    /// after the wait is killed first, so a failed test leaves no suspended process.
    fn exit_code_after_wait(watcher: &ChildProcess) -> Option<u32> {
        let code = watcher
            .wait_timeout(EXIT_WAIT)
            .expect("wait on the child succeeds");
        terminate_suspended(&watcher.process);
        code
    }

    /// Opens the job named `name` with the query right only.
    fn open_query_only(name: &str) -> JobObject {
        let wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
        // SAFETY: `wide` is nul-terminated and lives for the whole call. Inheritance is off.
        let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, wide.as_ptr()) };
        assert!(!raw.is_null(), "{}", io::Error::last_os_error());
        // SAFETY: `raw` is a non-null handle that OpenJobObjectW just returned, so
        // OwnedHandle takes sole ownership and closes it exactly once.
        JobObject::from_handle(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    /// Returns a job name that no other test run uses, from the process id and clock.
    fn unique_job_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        format!("mcao-unit-job-{}-{nanos}", process::id())
    }

    #[test]
    fn child_is_created_suspended() {
        let (child, watcher) = suspended_child();
        // SAFETY: `thread` is the live primary-thread handle that `child` owns.
        // SuspendThread returns the previous suspend count, which is 1 only if
        // CREATE_SUSPENDED left the thread suspended before any of its code ran.
        let previous_count = unsafe { SuspendThread(child.thread.as_raw_handle()) };
        // Kill the child before the assertion, so a failure cannot leak it.
        terminate_suspended(&child.process);
        exit_code_after_wait(&watcher);
        assert_eq!(
            previous_count, 1,
            "primary thread was not created suspended"
        );
    }

    #[test]
    fn failed_assignment_terminates_the_child_before_it_runs() {
        let name = unique_job_name();
        let _owner = JobObject::named(&name).expect("named job is created");
        let query_only = open_query_only(&name);
        let (child, watcher) = suspended_child();

        let refused = child.assign_to(&query_only).err();
        let code = exit_code_after_wait(&watcher);

        assert!(
            refused.is_some(),
            "a query-only job handle must refuse assignment"
        );
        assert_eq!(
            code,
            Some(SETUP_FAILED_EXIT),
            "the child was not terminated while suspended (a run to its end exits {RAN_TO_END})"
        );
    }

    #[test]
    fn failed_resume_terminates_the_child_before_it_runs() {
        let job = JobObject::new().expect("job object is created");
        let (child, watcher) = suspended_child();
        let assigned = child.assign_to(&job).expect("child joins the job");
        // A process handle is not a thread handle, so ResumeThread refuses it.
        let not_a_thread = assigned
            .process
            .try_clone()
            .expect("process handle duplicates");
        let broken = AssignedChild {
            thread: not_a_thread,
            ..assigned
        };

        let refused = broken.resume().err();
        let code = exit_code_after_wait(&watcher);

        assert!(
            refused.is_some(),
            "ResumeThread on a non-thread handle must fail"
        );
        assert_eq!(
            code,
            Some(SETUP_FAILED_EXIT),
            "the child was not terminated after the failed resume (a run to its end exits {RAN_TO_END})"
        );
    }
}
