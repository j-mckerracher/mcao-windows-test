//! Retry helpers for filesystem operations on Windows.
//!
//! Windows Defender and endpoint-detection software can transiently lock files or directories
//! immediately after creation or during deletion. This module provides exponential-backoff retry
//! loops for deleting directories and files safely.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::McaoError;

/// Configuration for exponential backoff retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of attempts allowed before giving up.
    pub max_attempts: u32,
    /// Base delay applied after the initial failure.
    pub base_delay: Duration,
    /// Maximum ceiling for the exponential delay.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// Computes the exponential backoff delay after a failed attempt.
    ///
    /// `attempt` is 1-based. Delay after failed attempt `n` is `base_delay * 2^(n-1)`,
    /// capped at `max_delay`. Attempt 0 behaves like attempt 1. Checked arithmetic is used
    /// so large values of `attempt` never overflow or panic.
    pub fn delay_after_failed_attempt(&self, attempt: u32) -> Duration {
        let effective_attempt = attempt.max(1);
        let shift = effective_attempt - 1;
        if self.base_delay.is_zero() {
            return Duration::ZERO;
        }
        let mut delay = self.base_delay;
        for _ in 0..shift.min(128) {
            match delay.checked_mul(2) {
                Some(next) if next <= self.max_delay => delay = next,
                _ => return self.max_delay,
            }
        }
        delay.min(self.max_delay)
    }
}

/// Determines whether an I/O error is considered transient and retryable.
///
/// Only Windows hosts retry: there, [`std::io::ErrorKind::PermissionDenied`] or a raw OS error of
/// 5 (`ERROR_ACCESS_DENIED`), 32 (`ERROR_SHARING_VIOLATION`), or 33 (`ERROR_LOCK_VIOLATION`) means
/// another process, usually Defender or an EDR agent, holds the file. On other platforms
/// `PermissionDenied` is a permanent condition such as a read-only parent, so nothing is retried,
/// matching the sharing-conflict rule in `state/store.rs`.
pub fn is_retryable(err: &std::io::Error) -> bool {
    cfg!(windows)
        && (err.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(err.raw_os_error(), Some(5 | 32 | 33)))
}

/// Executes an operation repeatedly according to a retry policy until it succeeds or exhausts attempts.
///
/// Calls `op` up to `policy.max_attempts` times (treating 0 as 1).
/// - On success, returns `Ok(val)` immediately.
/// - On an error `retryable` rejects, returns `Err(McaoError::Io(err))` immediately without sleeping.
/// - On a retryable error, calls `log(attempt, &err)`, then sleeps if attempts remain before retrying.
/// - After the last attempt fails, returns `Err(McaoError::Io(err))` without sleeping.
///
/// The public helpers in this module pass [`is_retryable`] as `retryable`.
pub fn retry_with<T>(
    policy: &RetryPolicy,
    mut op: impl FnMut() -> std::io::Result<T>,
    retryable: impl Fn(&std::io::Error) -> bool,
    mut sleep: impl FnMut(Duration),
    mut log: impl FnMut(u32, &std::io::Error),
) -> Result<T, McaoError> {
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match op() {
            Ok(val) => return Ok(val),
            Err(err) => {
                if !retryable(&err) {
                    return Err(McaoError::Io(err));
                }
                log(attempt, &err);
                if attempt < max_attempts {
                    sleep(policy.delay_after_failed_attempt(attempt));
                    attempt += 1;
                } else {
                    return Err(McaoError::Io(err));
                }
            }
        }
    }
}

/// Removes a directory tree, retrying when files are temporarily locked.
///
/// Uses the default retry policy, [`std::thread::sleep`], and logs each failed attempt to stderr.
pub fn remove_dir_with_retry(path: &Path) -> Result<(), McaoError> {
    remove_dir_with_retry_using(path, &RetryPolicy::default(), std::thread::sleep)
}

/// Removes a directory tree using the specified retry policy and sleep function.
///
/// A [`std::io::ErrorKind::NotFound`] error is treated as success (already removed).
pub fn remove_dir_with_retry_using(
    path: &Path,
    policy: &RetryPolicy,
    sleep: impl FnMut(Duration),
) -> Result<(), McaoError> {
    retry_with(
        policy,
        || match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        },
        is_retryable,
        sleep,
        |attempt, err| {
            eprintln!(
                "mcao: remove {} attempt {attempt} failed: {err}",
                path.display()
            );
        },
    )
}

/// Renames a target to a temporary sibling name and then removes it with retries.
///
/// Uses the default retry policy and [`std::thread::sleep`].
pub fn temp_rename_and_remove(target: &Path) -> Result<(), McaoError> {
    temp_rename_and_remove_using(target, &RetryPolicy::default(), std::thread::sleep)
}

/// Renames a target to a temporary sibling name and then removes it with retries, using
/// the specified policy and sleep function.
///
/// If `target` does not exist, returns `Ok(())`. Otherwise, renames `target` to
/// `.{file_name}.mcao-del-{pid}-{nanos}-{sequence}` and removes the renamed directory or file.
/// A [`std::io::ErrorKind::NotFound`] error from the rename or the removal counts as success.
/// If `target` has no file name, returns an [`std::io::ErrorKind::InvalidInput`] error.
///
/// When the removal fails after the rename succeeded, the original name is already gone and the
/// renamed sibling remains on disk. The returned error keeps the original kind and names that
/// sibling path so the caller can find and clean it up.
pub fn temp_rename_and_remove_using(
    target: &Path,
    policy: &RetryPolicy,
    sleep: impl FnMut(Duration),
) -> Result<(), McaoError> {
    temp_rename_and_remove_with(
        target,
        policy,
        sleep,
        |from, to| std::fs::rename(from, to),
        |path, is_dir| {
            if is_dir {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        },
    )
}

/// Builds the temporary sibling path that `target` is renamed to before removal.
///
/// The name carries the process id, the clock, and a per-process sequence number, so two calls
/// never produce the same path even when the clock does not advance.
fn temp_sibling_path(target: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let sibling_name = format!(
        ".{}.mcao-del-{pid}-{nanos}-{sequence}",
        file_name.to_string_lossy()
    );
    match target.parent() {
        Some(parent) => parent.join(sibling_name),
        None => PathBuf::from(sibling_name),
    }
}

/// [`temp_rename_and_remove_using`] with the filesystem operations injected, so a test can fail
/// the rename or the removal. `remove` receives the sibling path and whether it is a directory.
fn temp_rename_and_remove_with(
    target: &Path,
    policy: &RetryPolicy,
    mut sleep: impl FnMut(Duration),
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    mut remove: impl FnMut(&Path, bool) -> std::io::Result<()>,
) -> Result<(), McaoError> {
    let meta = match std::fs::symlink_metadata(target) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(McaoError::Io(err)),
    };
    let Some(file_name) = target.file_name() else {
        return Err(McaoError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no file name",
        )));
    };
    let sibling_path = temp_sibling_path(target, file_name);

    let renamed = retry_with(
        policy,
        || match rename(target, &sibling_path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        },
        is_retryable,
        &mut sleep,
        |attempt, err| {
            eprintln!(
                "mcao: rename {} attempt {attempt} failed: {err}",
                target.display()
            );
        },
    )?;
    if !renamed {
        return Ok(());
    }

    let is_dir = meta.is_dir();
    retry_with(
        policy,
        || match remove(&sibling_path, is_dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        },
        is_retryable,
        &mut sleep,
        |attempt, err| {
            eprintln!(
                "mcao: remove {} attempt {attempt} failed: {err}",
                sibling_path.display()
            );
        },
    )
    .map_err(|error| match error {
        McaoError::Io(err) => McaoError::Io(std::io::Error::new(
            err.kind(),
            format!(
                "renamed {} to {} but could not remove it, remove it by hand: {err}",
                target.display(),
                sibling_path.display()
            ),
        )),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_denied(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::PermissionDenied
    }

    #[test]
    fn delay_after_failed_attempt() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay_after_failed_attempt(1),
            Duration::from_millis(50)
        );
        assert_eq!(
            policy.delay_after_failed_attempt(2),
            Duration::from_millis(100)
        );
        assert_eq!(
            policy.delay_after_failed_attempt(3),
            Duration::from_millis(200)
        );
        assert_eq!(
            policy.delay_after_failed_attempt(6),
            Duration::from_millis(1600)
        );
        assert_eq!(policy.delay_after_failed_attempt(7), Duration::from_secs(2));
        assert_eq!(
            policy.delay_after_failed_attempt(200),
            Duration::from_secs(2)
        );
        assert_eq!(
            policy.delay_after_failed_attempt(0),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn is_retryable_cases() {
        let err_not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err_other = std::io::Error::other("other");
        assert!(!is_retryable(&err_not_found));
        assert!(!is_retryable(&err_other));
    }

    #[cfg(windows)]
    #[test]
    fn windows_retries_denied_and_sharing_codes() {
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(is_retryable(&denied));
        for code in [5, 32, 33] {
            assert!(
                is_retryable(&std::io::Error::from_raw_os_error(code)),
                "{code}"
            );
        }
        assert!(!is_retryable(&std::io::Error::from_raw_os_error(2)));
    }

    #[cfg(not(windows))]
    #[test]
    fn permission_denied_is_not_retried_off_windows() {
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(!is_retryable(&denied));

        let mut calls = 0;
        let mut sleeps = Vec::new();
        let result = retry_with(
            &RetryPolicy::default(),
            || {
                calls += 1;
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "read-only parent",
                ))
            },
            is_retryable,
            |d| sleeps.push(d),
            |_attempt, _err| panic!("a non-retryable error must not be logged as a retry"),
        );
        assert_eq!(calls, 1);
        assert!(sleeps.is_empty());
        assert!(matches!(result, Err(McaoError::Io(_))));
    }

    #[test]
    fn retry_with_succeeds_on_third_attempt() {
        let policy = RetryPolicy::default();
        let mut call_count = 0;
        let mut recorded_sleeps = Vec::new();
        let mut logged_attempts = Vec::new();

        let result = retry_with(
            &policy,
            || {
                call_count += 1;
                if call_count < 3 {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "busy",
                    ))
                } else {
                    Ok("done")
                }
            },
            is_denied,
            |d| recorded_sleeps.push(d),
            |attempt, _err| logged_attempts.push(attempt),
        );

        assert_eq!(result.unwrap(), "done");
        assert_eq!(
            recorded_sleeps,
            vec![Duration::from_millis(50), Duration::from_millis(100)]
        );
        assert_eq!(logged_attempts, vec![1, 2]);
    }

    #[test]
    fn retry_with_always_permission_denied() {
        let policy = RetryPolicy::default();
        let mut call_count = 0;
        let mut recorded_sleeps = Vec::new();
        let mut logged_attempts = Vec::new();

        let result = retry_with(
            &policy,
            || {
                call_count += 1;
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "locked",
                ))
            },
            is_denied,
            |d| recorded_sleeps.push(d),
            |attempt, _err| logged_attempts.push(attempt),
        );

        assert_eq!(call_count, 10);
        let expected_sleeps = vec![
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_millis(1600),
            Duration::from_millis(2000),
            Duration::from_millis(2000),
            Duration::from_millis(2000),
        ];
        assert_eq!(recorded_sleeps, expected_sleeps);
        assert_eq!(logged_attempts, (1..=10).collect::<Vec<_>>());
        match result {
            Err(McaoError::Io(err)) => {
                assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            }
            other => panic!("expected Err(McaoError::Io(PermissionDenied)), got {other:?}"),
        }
    }

    #[test]
    fn retry_with_non_retryable_error() {
        let policy = RetryPolicy::default();
        let mut call_count = 0;
        let mut recorded_sleeps = Vec::new();

        let result = retry_with(
            &policy,
            || {
                call_count += 1;
                Err::<(), _>(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
            },
            is_denied,
            |d| recorded_sleeps.push(d),
            |_attempt, _err| {},
        );

        assert_eq!(call_count, 1);
        assert!(recorded_sleeps.is_empty());
        match result {
            Err(McaoError::Io(err)) => {
                assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Err(McaoError::Io(NotFound)), got {other:?}"),
        }
    }

    #[test]
    fn retry_with_max_attempts_zero_calls_op_once() {
        let policy = RetryPolicy {
            max_attempts: 0,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(2),
        };
        let mut call_count = 0;
        let mut recorded_sleeps = Vec::new();

        let result = retry_with(
            &policy,
            || {
                call_count += 1;
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "denied",
                ))
            },
            is_denied,
            |d| recorded_sleeps.push(d),
            |_attempt, _err| {},
        );

        assert_eq!(call_count, 1);
        assert!(recorded_sleeps.is_empty());
        assert!(result.is_err());
    }

    #[test]
    fn remove_dir_with_retry_using_real_temp() {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_dir = std::env::temp_dir().join(format!("mcao-test-rmdir-{pid}-{nanos}"));
        let sub_dir = temp_dir.join("sub");
        std::fs::create_dir_all(&sub_dir).expect("create test dir");
        std::fs::write(sub_dir.join("file.txt"), "hello").expect("write file");

        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Cleanup(temp_dir.clone());

        let policy = RetryPolicy::default();
        let mut sleeps = Vec::new();
        let res = remove_dir_with_retry_using(&temp_dir, &policy, |d| sleeps.push(d));
        assert!(res.is_ok());
        assert!(!temp_dir.exists());

        // Calling again on missing path returns Ok (NotFound is success)
        let res2 = remove_dir_with_retry_using(&temp_dir, &policy, |d| sleeps.push(d));
        assert!(res2.is_ok());
    }

    #[test]
    fn temp_rename_and_remove_using_cases() {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!("mcao-test-rename-{pid}-{nanos}"));
        std::fs::create_dir_all(&base).expect("create base dir");

        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Cleanup(base.clone());

        let policy = RetryPolicy::default();

        // 1. Directory containing a file
        let dir_target = base.join("dir_target");
        std::fs::create_dir_all(&dir_target).expect("create dir_target");
        std::fs::write(dir_target.join("child.txt"), "content").expect("write child");

        let mut sleeps = Vec::new();
        let res = temp_rename_and_remove_using(&dir_target, &policy, |d| sleeps.push(d));
        assert!(res.is_ok());
        assert!(!dir_target.exists());

        // Verify no sibling .dir_target.mcao-del-* is left
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                assert!(
                    !name.starts_with(".dir_target.mcao-del-"),
                    "leftover sibling found: {name}"
                );
            }
        }

        // 2. Missing target returns Ok
        let missing = base.join("does_not_exist");
        let res_missing = temp_rename_and_remove_using(&missing, &policy, |d| sleeps.push(d));
        assert!(res_missing.is_ok());

        // 3. Plain file target is removed
        let file_target = base.join("plain_file.txt");
        std::fs::write(&file_target, "some data").expect("write file");
        assert!(file_target.exists());
        let res_file = temp_rename_and_remove_using(&file_target, &policy, |d| sleeps.push(d));
        assert!(res_file.is_ok());
        assert!(!file_target.exists());
    }

    #[test]
    fn temp_rename_and_remove_no_file_name() {
        let policy = RetryPolicy::default();
        let res = temp_rename_and_remove_using(Path::new("/"), &policy, |_| {});
        match res {
            Err(McaoError::Io(err)) => {
                assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
                assert_eq!(err.to_string(), "target has no file name");
            }
            other => panic!("expected Err(McaoError::Io(InvalidInput)), got {other:?}"),
        }
    }

    /// Removes its directory on drop, restoring permissions first so a failed test leaves nothing.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "mcao-test-{label}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            #[cfg(unix)]
            restore_permissions(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn restore_permissions(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                restore_permissions(&entry.path());
            }
        }
    }

    #[test]
    fn rename_not_found_counts_as_success() {
        let scratch = Scratch::new("rename-gone");
        let target = scratch.0.join("target");
        std::fs::write(&target, "x").expect("write target");

        let mut removed = false;
        let result = temp_rename_and_remove_with(
            &target,
            &RetryPolicy::default(),
            |_| {},
            |_, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "vanished",
                ))
            },
            |_, _| {
                removed = true;
                Ok(())
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(!removed, "nothing was renamed, so nothing is removed");
    }

    #[test]
    fn rename_failure_is_returned_and_leaves_the_target() {
        let scratch = Scratch::new("rename-fails");
        let target = scratch.0.join("target");
        std::fs::write(&target, "x").expect("write target");

        let mut removed = false;
        let result = temp_rename_and_remove_with(
            &target,
            &RetryPolicy::default(),
            |_| {},
            |_, _| Err(std::io::Error::other("rename refused")),
            |_, _| {
                removed = true;
                Ok(())
            },
        );
        match result {
            Err(McaoError::Io(err)) => assert_eq!(err.to_string(), "rename refused"),
            other => panic!("expected the rename error, got {other:?}"),
        }
        assert!(!removed);
        assert!(target.exists());
    }

    #[test]
    fn failed_removal_names_the_orphan_sibling() {
        let scratch = Scratch::new("orphan");
        let target = scratch.0.join("target");
        std::fs::create_dir_all(&target).expect("create target");

        let mut sibling = None;
        let result = temp_rename_and_remove_with(
            &target,
            &RetryPolicy::default(),
            |_| {},
            |from, to| {
                sibling = Some(to.to_path_buf());
                std::fs::rename(from, to)
            },
            |_, _| Err(std::io::Error::other("held open")),
        );
        let sibling = sibling.expect("rename ran");
        match result {
            Err(McaoError::Io(err)) => {
                let message = err.to_string();
                assert_eq!(err.kind(), std::io::ErrorKind::Other);
                assert!(
                    message.contains(&sibling.display().to_string()),
                    "{message}"
                );
                assert!(message.contains(&target.display().to_string()), "{message}");
                assert!(message.contains("held open"), "{message}");
            }
            other => panic!("expected the removal error, got {other:?}"),
        }
        assert!(sibling.exists(), "the orphan is still on disk");
        assert!(!target.exists());
    }

    #[test]
    fn sibling_names_are_unique_and_next_to_the_target() {
        let target = Path::new("some").join("dir").join("target");
        let name = target.file_name().expect("file name");
        let first = temp_sibling_path(&target, name);
        let second = temp_sibling_path(&target, name);

        assert_ne!(first, second);
        for sibling in [&first, &second] {
            assert_eq!(sibling.parent(), target.parent());
            let leaf = sibling.file_name().expect("leaf").to_string_lossy();
            assert!(leaf.starts_with(".target.mcao-del-"), "{leaf}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_removed_without_following_them() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("symlinks");
        let real_dir = scratch.0.join("real_dir");
        std::fs::create_dir_all(&real_dir).expect("create real dir");
        std::fs::write(real_dir.join("keep.txt"), "keep").expect("write keep");
        let real_file = scratch.0.join("real_file");
        std::fs::write(&real_file, "keep").expect("write real file");

        let dir_link = scratch.0.join("dir_link");
        let file_link = scratch.0.join("file_link");
        let dangling = scratch.0.join("dangling");
        symlink(&real_dir, &dir_link).expect("link dir");
        symlink(&real_file, &file_link).expect("link file");
        symlink(scratch.0.join("missing"), &dangling).expect("link nothing");

        for link in [&dir_link, &file_link, &dangling] {
            temp_rename_and_remove_using(link, &RetryPolicy::default(), |_| {})
                .unwrap_or_else(|err| panic!("{}: {err}", link.display()));
            assert!(
                std::fs::symlink_metadata(link).is_err(),
                "{}",
                link.display()
            );
        }
        assert!(
            real_dir.join("keep.txt").exists(),
            "directory link was followed"
        );
        assert!(real_file.exists(), "file link was followed");
    }
}
