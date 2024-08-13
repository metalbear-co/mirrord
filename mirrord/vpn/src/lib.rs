#![feature(concat_idents)]
#![feature(lazy_cell)]
#![feature(try_blocks)]

#[cfg(not(target_os = "macos"))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub mod agent;
pub mod config;
pub mod error;
pub mod packet;
pub mod socket;
pub mod tunnel;
