//! Shared profile policy and desktop paths. Applications own device resolution.

pub mod paths;
#[cfg(feature = "profile")]
pub mod profile;

pub const PROFILE_VERSION: u32 = 1;

pub fn new_profile_text() -> String {
    format!("version = {PROFILE_VERSION}\n")
}
