#![feature(lazy_cell)]

#[cfg(target_os = "macos")]
pub mod macos;

pub mod packet;
pub mod socket;
