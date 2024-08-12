#![feature(concat_idents)]
#![feature(lazy_cell)]

#[cfg(not(target_os = "macos"))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub mod agent;
pub mod config;
pub mod packet;
pub mod socket;
pub mod tunnel;
