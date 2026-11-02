//! Pure helpers ported from the Python reference programs in `bin/fm-awaiting-captain.sh`,
//! and from the millisecond clock formula the shell scripts use.
//!
//! Each submodule pins one contract. Nothing here starts a process, opens a file, or reads
//! the environment; the clock is the only input that is not a function of its arguments.

pub mod awaiting;
pub mod base64;
pub mod clock;
pub mod json;
pub mod path;
pub mod pystr;
