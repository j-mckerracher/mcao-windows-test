//! Sends `CTRL_BREAK_EVENT` to a child that is attached to a pseudoconsole.
//!
//! `GenerateConsoleCtrlEvent` delivers an event only to processes that share the
//! caller's console. A ConPTY child has its own console, so the caller must leave
//! its console, attach to the child's, send the event, and leave again. All of that
//! changes the console of the whole calling process, not of one thread. The
//! functions here hold a lock so that two callers in this process cannot interleave
//! the steps. They cannot protect other code in the process that prints to the
//! console while the process is detached. Such a print can fail.
//!
//! The target must have been started with `CREATE_NEW_PROCESS_GROUP`
//! ([`ProcessGroup::New`](crate::process::ProcessGroup::New)), so that its process
//! id is a group id and the event reaches that group and no other process.
//!
//! All `unsafe` console calls of the interrupt ladder live in this file.

use std::io;
use std::sync::Mutex;

use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
    GetConsoleProcessList,
};

/// Serializes the detach, attach, send and restore steps within this process.
static CONSOLE_STEPS: Mutex<()> = Mutex::new(());

/// What a delivered `CTRL_BREAK_EVENT` left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtrlBreakSent {
    /// Set when the event was delivered but the calling process could not leave the
    /// child's console or return to its parent's console. The process may be left
    /// without a console. The text is the OS error from the first failing step.
    pub console_warning: Option<String>,
}

/// Sends `CTRL_BREAK_EVENT` to the process group led by `group_id`.
///
/// The calling process leaves its own console for the duration of the call and
/// returns to its parent's console afterwards, if it had a console at all. A
/// process that attached to a console of its own, rather than inheriting its
/// parent's, ends up on the parent's console instead.
///
/// # Errors
///
/// Returns the OS error from the first step that stops the event from being
/// delivered: leaving the current console, attaching to the child's console, or
/// `GenerateConsoleCtrlEvent`. Returns an error without touching the console when
/// an earlier call panicked and poisoned the lock. A failure to leave the child's
/// console or to return to the parent's console happens after delivery, so it is
/// not an error: it is reported in [`CtrlBreakSent::console_warning`].
pub fn send_ctrl_break(group_id: u32) -> io::Result<CtrlBreakSent> {
    let _steps = CONSOLE_STEPS
        .lock()
        .map_err(|_| io::Error::other("the console steps lock was poisoned by a panic"))?;
    let had_console = has_console();
    if had_console {
        free_console()?;
    }
    let attached = attach(group_id);
    let was_attached = attached.is_ok();
    let sent = attached.and_then(|()| generate_break(group_id));
    let detached = if was_attached { free_console() } else { Ok(()) };
    let restored = if had_console {
        attach(ATTACH_PARENT_PROCESS)
    } else {
        Ok(())
    };
    settle(sent, detached, restored)
}

/// Combines the step results of [`send_ctrl_break`]. Only the delivery result
/// decides success. A console step that failed after delivery becomes a warning.
fn settle(
    sent: io::Result<()>,
    detached: io::Result<()>,
    restored: io::Result<()>,
) -> io::Result<CtrlBreakSent> {
    sent?;
    let console_warning = detached.and(restored).err().map(|error| error.to_string());
    Ok(CtrlBreakSent { console_warning })
}

/// Reports whether the calling process is attached to a console.
fn has_console() -> bool {
    let mut one_process = 0_u32;
    // SAFETY: The pointer is to one writable `u32` and the count says one entry.
    // The call returns 0 when the process has no console.
    unsafe { GetConsoleProcessList(&mut one_process, 1) != 0 }
}

/// Detaches the calling process from its console.
fn free_console() -> io::Result<()> {
    // SAFETY: The call takes no arguments and only changes this process's console.
    if unsafe { FreeConsole() } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Attaches the calling process to the console of `process_id`, or to its
/// parent's console for `ATTACH_PARENT_PROCESS`.
fn attach(process_id: u32) -> io::Result<()> {
    // SAFETY: The argument is a plain integer. The call fails, and changes
    // nothing, when the process is already attached to a console.
    if unsafe { AttachConsole(process_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `CTRL_BREAK_EVENT` to `group_id` on the console the process is attached to.
fn generate_break(group_id: u32) -> io::Result<()> {
    // SAFETY: Both arguments are plain integers. The call reads no memory.
    if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(text: &str) -> io::Result<()> {
        Err(io::Error::other(text.to_owned()))
    }

    #[test]
    fn a_clean_send_has_no_warning() {
        let sent = settle(Ok(()), Ok(()), Ok(())).expect("the event was delivered");
        assert_eq!(sent.console_warning, None);
    }

    #[test]
    fn a_delivered_event_survives_a_failed_restore() {
        let sent = settle(Ok(()), Ok(()), failure("no parent console"))
            .expect("the event was delivered, so this is not an error");
        assert_eq!(sent.console_warning.as_deref(), Some("no parent console"));
    }

    #[test]
    fn a_delivered_event_survives_a_failed_detach() {
        let sent = settle(Ok(()), failure("detach"), failure("restore"))
            .expect("the event was delivered, so this is not an error");
        assert_eq!(sent.console_warning.as_deref(), Some("detach"));
    }

    #[test]
    fn an_event_that_was_not_delivered_is_an_error_even_if_the_restore_worked() {
        let error =
            settle(failure("attach failed"), Ok(()), Ok(())).expect_err("nothing was delivered");
        assert_eq!(error.to_string(), "attach failed");
    }
}
