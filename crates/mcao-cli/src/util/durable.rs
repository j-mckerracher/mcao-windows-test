//! `mcao util durable-sync <path>`: flushes a file, then its parent directory, to stable storage.
//!
//! A port of the python3 `durable_sync` helper in `bin/fm-local-handoff.sh`. On macOS the flush
//! asks for F_FULLFSYNC, which also drains the drive cache, and falls back to fsync where the
//! filesystem refuses that request. Elsewhere on unix it is fsync. Other platforms refuse,
//! because the reference needs the `fcntl` module that Windows lacks.

use std::env;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

pub fn run(path: &str) -> ExitCode {
    match durable_sync(Path::new(path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mcao util: durable-sync {path}: {err}");
            ExitCode::from(1)
        }
    }
}

fn durable_sync(path: &Path) -> io::Result<()> {
    let parent = parent_dir(path)?;
    for target in [path, parent.as_path()] {
        sync_file(&File::open(target)?)?;
    }
    Ok(())
}

/// `os.path.dirname(os.path.abspath(path))`: the absolute path with `.` and `..` resolved
/// lexically, then its parent. Symbolic links are not resolved, as in Python.
fn parent_dir(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut normal = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    Ok(normal
        .parent()
        .map_or_else(|| normal.clone(), Path::to_path_buf))
}

#[cfg(unix)]
fn sync_file(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        // SAFETY: F_FULLFSYNC takes no argument, and `file` keeps `fd` open for the call.
        if unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if !matches!(
            err.raw_os_error(),
            Some(libc::ENOTSUP | libc::EINVAL | libc::ENOTTY)
        ) {
            return Err(err);
        }
    }
    // SAFETY: `file` keeps `fd` open for the call.
    if unsafe { libc::fsync(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn sync_file(_file: &File) -> io::Result<()> {
    Err(io::Error::other("not implemented on this platform"))
}
