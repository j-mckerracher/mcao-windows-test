//! Integration tests for the Job Object wrapper and the suspended spawn.
//!
//! The stub processes are this test binary re-run with `--exact <stub> --ignored`,
//! so the tests need no extra binary and no shell. The stub tests are ignored and
//! run only through that re-exec.

#![cfg(windows)]

use std::env;
use std::io;
use std::iter;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::{self, Command};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mcao_backend_windows::job::JobObject;
use mcao_backend_windows::process::spawn_in_job;
use windows_sys::Win32::System::JobObjects::{IsProcessInJob, OpenJobObjectW};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::core::BOOL;

/// Exit code given to the job, and therefore to the stub child.
const STUB_EXIT: u32 = 42;
/// Long enough that the job, not the stub's own timeout, ends each stub.
const STUB_SLEEP: Duration = Duration::from_secs(120);
/// `JOB_OBJECT_QUERY` access right from the Windows SDK. It lets a handle ask a job
/// for its members. Declared here so the test needs no extra windows-sys feature.
const JOB_OBJECT_QUERY: u32 = 0x0004;
/// Exit code the probe child uses when its code runs inside the named job.
const PROBE_IN_JOB: i32 = 77;
/// Exit code the probe child uses when its code runs outside the named job.
const PROBE_OUTSIDE_JOB: i32 = 78;

fn this_exe() -> PathBuf {
    env::current_exe().expect("test binary path is available")
}

/// Terminates the job when dropped, so a failed assertion cannot leak stubs.
struct TerminateOnDrop<'a>(&'a JobObject);

impl Drop for TerminateOnDrop<'_> {
    fn drop(&mut self) {
        // Best effort: the job may already be empty.
        let _ = self.0.terminate(1);
    }
}

/// Polls `condition` every 50 ms until it holds or `timeout` expires.
fn poll_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Re-exec stub: starts a grandchild, which joins this job, then waits.
#[test]
#[ignore = "re-exec stub; run only by job_tree_is_terminated_as_one"]
fn stub_child() {
    let mut grandchild = Command::new(this_exe())
        .args(["--exact", "stub_grandchild", "--ignored"])
        .spawn()
        .expect("stub child starts its grandchild");
    // The job terminates both processes, which ends this wait.
    grandchild
        .wait()
        .expect("stub child waits on its grandchild");
}

/// Re-exec stub: waits so the job holds a live descendant.
#[test]
#[ignore = "re-exec stub; run only by job_tree_is_terminated_as_one"]
fn stub_grandchild() {
    thread::sleep(STUB_SLEEP);
}

/// Re-exec stub. The job name is its last argument. It exits with `PROBE_IN_JOB`
/// only if its own code runs inside that named job. A run that matches no test
/// exits 0, and a panic exits 101, so neither can pass for the sentinel.
#[test]
#[ignore = "re-exec stub; run only by child_is_in_job_when_its_code_runs"]
fn stub_probe_job_membership() {
    let name = env::args()
        .next_back()
        .expect("parent passes the job name as the last argument");
    let in_job = current_process_in_named_job(&name).expect("child queries the named job");
    process::exit(if in_job {
        PROBE_IN_JOB
    } else {
        PROBE_OUTSIDE_JOB
    });
}

/// Reports whether the current process belongs to the job named `name`.
///
/// Asking about one named job, rather than any job, means a job inherited from
/// cargo or the test runner cannot satisfy the check.
fn current_process_in_named_job(name: &str) -> io::Result<bool> {
    let wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
    // SAFETY: `wide` is nul-terminated and lives for the whole call. Inheritance is off.
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, wide.as_ptr()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a non-null handle that OpenJobObjectW just returned, so
    // OwnedHandle takes sole ownership and closes it exactly once.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut in_job: BOOL = 0;
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that never needs closing.
    // `job` is live for the call, and `in_job` is a valid out pointer.
    let ok = unsafe { IsProcessInJob(GetCurrentProcess(), job.as_raw_handle(), &mut in_job) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(in_job != 0)
}

/// Returns a job name that no other test run uses, from the process id and clock.
fn unique_job_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    format!("mcao-job-membership-{}-{nanos}", process::id())
}

/// The child's code must run inside the job the parent assigned it to.
///
/// The probe asks the named job, so a job inherited from cargo cannot satisfy it.
/// If assignment is skipped or sends the child elsewhere, the child runs outside
/// this job and exits with `PROBE_OUTSIDE_JOB`, which fails the test. That result
/// does not depend on timing. Ordering is enforced by the types in `process.rs`,
/// because `resume` exists only after assignment.
#[test]
fn child_is_in_job_when_its_code_runs() {
    let name = unique_job_name();
    let job = JobObject::named(&name).expect("named job is created");
    let _guard = TerminateOnDrop(&job);
    let child = spawn_in_job(
        &job,
        &this_exe(),
        &[
            "--exact",
            "stub_probe_job_membership",
            "--ignored",
            name.as_str(),
        ],
    )
    .expect("probe child spawns into the job");

    let code = child
        .wait_timeout(Duration::from_secs(30))
        .expect("child wait succeeds");
    assert_eq!(
        code,
        Some(PROBE_IN_JOB.unsigned_abs()),
        "child code did not run inside the named job (exit code {code:?}; 0 means no test matched the stub)"
    );
}

#[test]
fn new_job_has_no_processes() {
    let job = JobObject::new().expect("job object is created");
    assert_eq!(job.active_processes().expect("job is queried"), 0);
}

#[test]
fn job_tree_is_terminated_as_one() {
    let job = JobObject::new().expect("job object is created");
    let _guard = TerminateOnDrop(&job);
    let child = spawn_in_job(&job, &this_exe(), &["--exact", "stub_child", "--ignored"])
        .expect("stub child spawns into the job");

    let tree_formed = poll_until(Duration::from_secs(30), || {
        job.active_processes().is_ok_and(|n| n == 2)
    });
    assert!(
        tree_formed,
        "child and grandchild never both joined the job"
    );
    assert_eq!(job.active_processes().expect("job is queried"), 2);

    job.terminate(STUB_EXIT).expect("job is terminated");

    let emptied = poll_until(Duration::from_secs(10), || {
        job.active_processes().is_ok_and(|n| n == 0)
    });
    assert!(emptied, "job still has live processes after termination");
    assert_eq!(
        child
            .wait_timeout(Duration::from_secs(10))
            .expect("child wait succeeds"),
        Some(STUB_EXIT)
    );
}

#[test]
fn spawn_of_missing_program_is_error() {
    let job = JobObject::new().expect("job object is created");
    let missing = PathBuf::from(r"C:\mcao-no-such-dir\mcao-no-such-program.exe");
    let err = spawn_in_job(&job, &missing, &[]).expect_err("missing program is refused");
    assert!(
        err.raw_os_error().is_some(),
        "refusal comes from CreateProcessW, not from local validation: {err}"
    );
    assert_eq!(job.active_processes().expect("job is queried"), 0);
}
