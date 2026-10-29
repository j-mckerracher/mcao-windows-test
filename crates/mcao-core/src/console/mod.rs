//! Worker console observation: the bounded output ring buffer and the framed wire codec. Pure std; the pipe server and commands arrive in later slices.

pub mod frame;
pub mod ring;
