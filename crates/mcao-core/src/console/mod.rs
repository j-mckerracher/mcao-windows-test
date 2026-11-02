//! Worker console observation: the bounded output ring buffer, the framed wire codec, and the per-connection read-only session logic. Pure std; the pipe server and commands arrive in later slices.

pub mod frame;
pub mod ring;
pub mod session;
