//! Windows-specific process utilities
//!
//! This module provides Windows-specific process functionality including
//! process execution with layer injection and parent/child synchronization.

pub mod command_line;
pub mod console;
pub mod diagnostics;
pub mod execution;
pub mod injection;
pub mod sync;
