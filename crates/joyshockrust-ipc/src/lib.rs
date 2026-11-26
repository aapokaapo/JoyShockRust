//! JoyShockRust's local wire formats. The applications own I/O and scheduling.

pub mod control;
pub mod status;
pub mod telemetry;
#[cfg(target_os = "linux")]
pub mod unix;

pub const STATUS_SOCKET: &str = "status.sock";
pub const RELOAD_SOCKET: &str = "reload.sock";
pub const LOAD_SUFFIX: &str = ".load";
pub const DEFAULT_LOAD_SOCKET: &str = "reload.sock.load";
