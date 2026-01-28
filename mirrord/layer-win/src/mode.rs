use crate::{ide_only::is_ide_only_mode, trace_only::is_trace_only_mode};

/// The execution mode of the mirrord layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// IDE-only mode: only process hooks for DLL propagation, no proxy connection,
    /// no config. The IDE manages the mirrord environment.
    Ide,
    /// Trace-only mode: hooks enabled but no agent/proxy connection.
    /// Useful for debugging hook behavior.
    TraceOnly,
    /// Normal mode: full mirrord functionality with proxy and agent connection.
    /// This is the default when you run `mirrord exec` normally.
    Normal,
}

impl ExecutionMode {
    /// Determine the current execution mode based on environment state.
    pub fn detect() -> Self {
        if is_ide_only_mode() {
            Self::Ide
        } else if is_trace_only_mode() {
            Self::TraceOnly
        } else {
            Self::Normal
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ide => write!(f, "IDE-only"),
            Self::TraceOnly => write!(f, "trace-only"),
            Self::Normal => write!(f, "normal"),
        }
    }
}
