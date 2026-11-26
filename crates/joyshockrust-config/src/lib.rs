//! Shared profile compilation, persistence and desktop paths.

#[cfg(feature = "profile")]
pub mod controller;
#[cfg(all(feature = "linux-profile", target_os = "linux"))]
pub mod linux_keys;
pub mod paths;
#[cfg(feature = "profile")]
pub mod profile;
#[cfg(all(feature = "linux-profile", target_os = "linux"))]
pub mod profile_file;

pub const PROFILE_VERSION: u32 = 1;

pub fn new_profile_text() -> String {
    format!("version = {PROFILE_VERSION}\n")
}
