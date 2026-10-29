//! ConPTY: a Windows pseudoconsole that a child process uses as its terminal.
//!
//! [`ConPty::open`] creates the pseudoconsole and two pipes. The caller writes
//! keystrokes to [`ConPtyIo::input`] and reads the terminal output, a stream of
//! text and VT escape sequences, from [`ConPtyIo::output`]. A child attaches to
//! the pseudoconsole through a process attribute, built by
//! `ConPty::attributes` for the spawn code in `process.rs`.
//!
//! # Terminal mechanism decision (DPES-752 slice 752.C)
//!
//! This module calls the ConPTY API directly through `windows-sys`. It does not
//! use the `portable-pty` crate, which was approved for evaluation only. Reasons:
//!
//! - **Job ownership.** `portable-pty` 0.9.0 creates the child with
//!   `CreateProcessW` and no `CREATE_SUSPENDED`. The child runs before the
//!   caller can assign it to a Job Object, so a grandchild created in that gap
//!   escapes the job. Slice 752.B placed ownership in the job, and an unowned
//!   grandchild would break its cleanup contract. Direct calls keep the
//!   suspended-then-assign-then-resume order.
//! - **Platform leakage.** `portable-pty` 0.9.0 declares `nix` as an
//!   unconditional dependency, so `cargo tree --target x86_64-pc-windows-msvc`
//!   lists it. The workspace rule is that Windows builds must not link `nix`.
//! - **Dependencies.** `portable-pty` adds 13 crates that `Cargo.lock` does not
//!   hold today (`anyhow`, `cfg-if`, `downcast-rs`, `filedescriptor`,
//!   `lazy_static`, `log`, `nix`, `serial2`, `shared_library`, `shell-words`,
//!   `thiserror`, `thiserror-impl`, `winreg`), and it uses `winapi` beside
//!   `windows-sys`. Direct calls need no new crate.
//! - **Size of the unsafe surface.** The direct code is two pipes, one
//!   pseudoconsole handle, and one attribute list, all behind safe wrappers with
//!   a `SAFETY:` comment per call.
//!
//! Because `portable-pty` did not win, no second crate approval is needed.
//!
//! One lesson is taken from `portable-pty`: the child is started with explicit
//! invalid standard handles, so it cannot inherit the parent's redirected stdio
//! in place of the pseudoconsole. See `process.rs`.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::marker::PhantomData;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use mcao_core::terminal::resolve_window_size;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, COORD, ClosePseudoConsole, CreatePseudoConsole,
    GetConsoleScreenBufferInfo, GetConsoleWindow, GetStdHandle, HPCON, STD_OUTPUT_HANDLE,
    SetConsoleCP, SetConsoleOutputCP,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, UpdateProcThreadAttribute,
};

/// `HRESULT_FROM_WIN32` marks its values with this facility prefix.
const HRESULT_WIN32_PREFIX: u32 = 0x8007_0000;
const HRESULT_FACILITY_MASK: u32 = 0xFFFF_0000;
const HRESULT_CODE_MASK: u32 = 0x0000_FFFF;

/// The size of a pseudoconsole in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleSize {
    /// Width in columns. Must be between 1 and `i16::MAX`.
    pub columns: u16,
    /// Height in rows. Must be between 1 and `i16::MAX`.
    pub rows: u16,
}

impl ConsoleSize {
    /// Converts to the `COORD` that `CreatePseudoConsole` takes.
    ///
    /// The API reads the fields as signed 16-bit values, so a dimension above
    /// `i16::MAX` would turn negative. A zero dimension is refused as well.
    fn to_coord(self) -> io::Result<COORD> {
        let dimension = |value: u16| {
            i16::try_from(value)
                .ok()
                .filter(|&cells| cells > 0)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "console size must be between 1 and 32767 in each dimension",
                    )
                })
        };
        Ok(COORD {
            X: dimension(self.columns)?,
            Y: dimension(self.rows)?,
        })
    }
}

/// The caller's ends of the pseudoconsole pipes.
#[derive(Debug)]
pub struct ConPtyIo {
    /// Keystrokes written here reach the child as terminal input.
    ///
    /// A write blocks while the pipe is full, which happens only when the child
    /// is not reading its input.
    pub input: File,
    /// Terminal output arrives here. A read returns `Ok(0)` once the
    /// pseudoconsole is closed and its buffered output is read.
    pub output: File,
}

/// An owned pseudoconsole.
///
/// Dropping the value calls `ClosePseudoConsole`. Before Windows 11 24H2 that
/// call waits until the output pipe is drained, so a reader must keep reading
/// [`ConPtyIo::output`] while the value is dropped.
#[derive(Debug)]
pub struct ConPty {
    handle: HPCON,
}

impl ConPty {
    /// Creates a pseudoconsole of `size` and the pipes that feed it.
    ///
    /// When this process has a console window, the console code pages are set to
    /// UTF-8 first; see [`init_console_encoding`].
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `size` has a zero dimension or one above
    /// `i16::MAX`. Returns the OS error from the code page calls, from `CreatePipe`,
    /// or an error that names the `HRESULT` from `CreatePseudoConsole`.
    pub fn open(size: ConsoleSize) -> io::Result<(Self, ConPtyIo)> {
        let coord = size.to_coord()?;
        if has_console_window() {
            init_console_encoding()?;
        }
        let (input_read, input_write) = create_pipe()?;
        let (output_read, output_write) = create_pipe()?;
        let mut handle: HPCON = 0;
        // SAFETY: Both pipe handles are valid and owned by this function for the
        // whole call. `handle` is a valid out pointer. Flags 0 request no cursor
        // inheritance, so the host sends no cursor-position query.
        let result = unsafe {
            CreatePseudoConsole(
                coord,
                input_read.as_raw_handle(),
                output_write.as_raw_handle(),
                0,
                &mut handle,
            )
        };
        if result < 0 {
            return Err(hresult_error("CreatePseudoConsole", result));
        }
        // The pseudoconsole holds its own duplicates of the two ends it uses.
        // Closing ours matters for output: while this process keeps the write end
        // open, the read end would never report end of file.
        drop(input_read);
        drop(output_write);
        let io = ConPtyIo {
            input: File::from(input_write),
            output: File::from(output_read),
        };
        Ok((Self { handle }, io))
    }

    /// Builds the process attribute list that attaches a new child to this
    /// pseudoconsole. The list borrows the pseudoconsole, so it cannot outlive it.
    ///
    /// # Errors
    ///
    /// Returns the OS error from `InitializeProcThreadAttributeList` or
    /// `UpdateProcThreadAttribute`.
    pub(crate) fn attributes(&self) -> io::Result<ConsoleAttributes<'_>> {
        let mut size: usize = 0;
        // SAFETY: A null list with a valid size pointer is the documented way to
        // ask how many bytes one attribute needs. The call reads no list memory.
        let probe = unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        if probe != 0 {
            return Err(io::Error::other(
                "InitializeProcThreadAttributeList accepted a null list",
            ));
        }
        let probe_error = io::Error::last_os_error();
        if probe_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || size == 0 {
            return Err(probe_error);
        }
        // The list is opaque storage that holds pointers, so it is allocated as
        // u64 words to get at least pointer alignment.
        let mut storage = vec![0_u64; size.div_ceil(mem::size_of::<u64>())];
        // SAFETY: `storage` is at least `size` bytes, as the probe reported, and
        // is aligned for pointers. `size` is a valid in-out pointer.
        let ok = unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut size)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // From here the list is initialized, so Drop must delete it, including on
        // the error return below.
        let mut attributes = ConsoleAttributes {
            storage,
            console: PhantomData,
        };
        // SAFETY: The list is initialized and has room for one attribute. For
        // PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE the value is the HPCON itself, not
        // a pointer to it, and its size is the size of an HPCON. `self` outlives
        // the returned list through the `'console` borrow, and the OS copies the
        // value into the list during this call.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                attributes.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                self.handle as *const c_void,
                mem::size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(attributes)
    }
}

impl Drop for ConPty {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful CreatePseudoConsole and this is
        // the only owner, so it is closed exactly once.
        unsafe { ClosePseudoConsole(self.handle) };
    }
}

/// An initialized process attribute list that names one pseudoconsole.
///
/// Dropping the value calls `DeleteProcThreadAttributeList`.
pub(crate) struct ConsoleAttributes<'console> {
    storage: Vec<u64>,
    console: PhantomData<&'console ConPty>,
}

impl ConsoleAttributes<'_> {
    /// Returns the raw list for `STARTUPINFOEXW::lpAttributeList`.
    ///
    /// The pointer is valid while this value is alive and not moved out of.
    pub(crate) fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for ConsoleAttributes<'_> {
    fn drop(&mut self) {
        // SAFETY: The list was initialized before this value was built, and the
        // pseudoconsole value it copied is not read here. It is deleted once.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

/// Creates an anonymous pipe with default security and a default buffer size.
///
/// The handles are not inheritable, so no child gets them by accident.
fn create_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    // SAFETY: Both out pointers are valid. The null security attributes select
    // default security and non-inheritable handles. Size 0 selects the default
    // buffer size.
    let ok = unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe succeeded, so both handles are new and nothing else
    // owns them. Each OwnedHandle closes its handle exactly once.
    let (read, write) = unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    };
    Ok((read, write))
}

/// Turns a failed `HRESULT` into an `io::Error` that names the call.
///
/// A value made by `HRESULT_FROM_WIN32` maps back to its Win32 error, which
/// carries the system message. Any other value is reported in hexadecimal.
fn hresult_error(call: &str, result: i32) -> io::Error {
    let bits = result as u32;
    if bits & HRESULT_FACILITY_MASK == HRESULT_WIN32_PREFIX {
        let code = i32::try_from(bits & HRESULT_CODE_MASK).expect("a 16-bit code fits in i32");
        let source = io::Error::from_raw_os_error(code);
        return io::Error::new(source.kind(), format!("{call} failed: {source}"));
    }
    io::Error::other(format!("{call} failed with HRESULT {bits:#010x}"))
}

/// The UTF-8 code page, which the console and the child use for text.
const CODE_PAGE_UTF8: u32 = 65001;

/// The environment variable that names the pseudoconsole size as `COLSxROWS`.
const SIZE_ENV: &str = "SIZE";

/// Sets the console input and output code pages to UTF-8.
///
/// The code pages are console state, not process state. They apply to the console this
/// process is attached to, which the launching shell shares and which outlives this
/// process, so the change stays in place after the process exits. Call it only when this
/// process has a console window: a headless process has none to configure, and the calls
/// fail.
///
/// # Errors
///
/// Returns the OS error from `SetConsoleOutputCP` or `SetConsoleCP`.
pub fn init_console_encoding() -> io::Result<()> {
    // SAFETY: SetConsoleOutputCP takes a code page value and no pointers. It changes the
    // output code page of the console this process is attached to, which every process on
    // that console shares.
    if unsafe { SetConsoleOutputCP(CODE_PAGE_UTF8) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: SetConsoleCP takes a code page value and no pointers. It changes the input
    // code page of the same shared console.
    if unsafe { SetConsoleCP(CODE_PAGE_UTF8) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reports whether this process has a console window.
///
/// A headless process, such as a service or a CI runner, has none.
fn has_console_window() -> bool {
    // SAFETY: GetConsoleWindow takes no arguments and reads no memory.
    !unsafe { GetConsoleWindow() }.is_null()
}

/// Returns the visible size of the console behind this process's standard output as
/// `(columns, rows)`, or `None` when that handle is not a console or its size cannot be
/// read. A process whose standard output is redirected returns `None` even when a console
/// window exists, so the caller falls back to the default size.
pub fn console_window_size() -> Option<(u16, u16)> {
    // SAFETY: GetStdHandle takes a constant and reads no memory. An error value
    // makes the next call fail rather than read through it.
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: `info` is a live, writable CONSOLE_SCREEN_BUFFER_INFO, and the call
    // writes no more than that structure.
    if unsafe { GetConsoleScreenBufferInfo(output, &mut info) } == 0 {
        return None;
    }
    let window = info.srWindow;
    let columns = i32::from(window.Right) - i32::from(window.Left) + 1;
    let rows = i32::from(window.Bottom) - i32::from(window.Top) + 1;
    Some((u16::try_from(columns).ok()?, u16::try_from(rows).ok()?))
}

/// Picks the size to open the pseudoconsole at.
///
/// The `SIZE` environment variable, as `COLSxROWS`, wins when it parses. Otherwise
/// the console window is used, and otherwise the default. The result is never below
/// 80x24.
pub fn negotiated_size() -> ConsoleSize {
    let env = std::env::var(SIZE_ENV).ok();
    let (columns, rows) = resolve_window_size(env.as_deref(), console_window_size());
    ConsoleSize { columns, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_converts_to_coord() {
        let coord = ConsoleSize {
            columns: 120,
            rows: 30,
        }
        .to_coord()
        .expect("a normal size converts");
        assert_eq!((coord.X, coord.Y), (120, 30));
    }

    #[test]
    fn zero_and_oversize_dimensions_are_refused() {
        for (columns, rows) in [(0, 24), (80, 0), (32768, 24), (80, 32768), (u16::MAX, 1)] {
            // COORD has no Debug, so the result is matched instead of unwrapped.
            let refused = matches!(
                ConsoleSize { columns, rows }.to_coord(),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput
            );
            assert!(refused, "{columns}x{rows} is refused as invalid input");
        }
    }

    #[test]
    fn largest_dimension_is_accepted() {
        let coord = ConsoleSize {
            columns: 32767,
            rows: 1,
        }
        .to_coord()
        .expect("i16::MAX is allowed");
        assert_eq!((coord.X, coord.Y), (i16::MAX, 1));
    }

    #[test]
    fn win32_hresult_maps_to_its_os_error() {
        // E_INVALIDARG is HRESULT_FROM_WIN32(ERROR_INVALID_PARAMETER).
        let error = hresult_error("Call", 0x8007_0057_u32 as i32);
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().starts_with("Call failed: "));
    }

    #[test]
    fn other_hresult_is_reported_in_hex() {
        let error = hresult_error("Call", 0x8000_4005_u32 as i32);
        assert_eq!(error.to_string(), "Call failed with HRESULT 0x80004005");
    }

    #[test]
    fn open_and_close_without_a_child() {
        let (console, io) = ConPty::open(ConsoleSize {
            columns: 80,
            rows: 24,
        })
        .expect("a pseudoconsole opens");
        // No child ever attached, so there is no output to drain on close.
        drop(console);
        drop(io);
    }

    #[test]
    fn attributes_build_for_an_open_console() {
        let (console, _io) = ConPty::open(ConsoleSize {
            columns: 80,
            rows: 24,
        })
        .expect("a pseudoconsole opens");
        let mut attributes = console.attributes().expect("the attribute list builds");
        assert!(!attributes.as_mut_ptr().is_null());
    }

    #[test]
    fn negotiated_size_never_falls_below_the_minimum() {
        use mcao_core::terminal::MIN_WINDOW;

        let size = negotiated_size();
        assert!(size.columns >= MIN_WINDOW.0, "columns {}", size.columns);
        assert!(size.rows >= MIN_WINDOW.1, "rows {}", size.rows);
    }

    #[test]
    fn a_console_window_reports_a_usable_size() {
        // A headless runner has no console, so there is nothing to measure. The
        // check runs only where a console exists.
        if let Some((columns, rows)) = console_window_size() {
            assert!(columns > 0 && rows > 0, "{columns}x{rows}");
        }
    }

    #[test]
    fn init_sets_the_utf8_output_code_page_when_a_console_exists() {
        use windows_sys::Win32::System::Console::GetConsoleOutputCP;

        // Skipped on a headless runner, where there is no console window. This test does
        // not cover the headless CI path: `tests/conpty.rs` runs the init on a ConPTY
        // console, which exists headless, and asserts both code pages read back as UTF-8.
        if !has_console_window() {
            return;
        }
        init_console_encoding().expect("the console accepts UTF-8");
        // SAFETY: GetConsoleOutputCP takes no arguments and reads no memory.
        assert_eq!(unsafe { GetConsoleOutputCP() }, CODE_PAGE_UTF8);
    }
}
