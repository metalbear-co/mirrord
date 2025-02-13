use std::path::{Path, PathBuf};

/// Represents a [`Path`] that was possibly mapped to another one.
///
/// This is a convenience trait to allows this crate for producing nice error messages.
/// In the context of mirrord-agent TLS configuration, the user specifies paths from the target
/// container filesystem. The agent resolves them to real paths, using target container filesystem
/// root.
pub trait MaybeMappedPath {
    /// Returns the real [`Path`] that should be accessed in the code.
    fn real_path(&self) -> &Path;

    /// Returns the [`Path`] that should be displayed to the user, e.g in error messages.
    fn display_path(&self) -> &Path;
}

impl MaybeMappedPath for Path {
    fn real_path(&self) -> &Path {
        self
    }

    fn display_path(&self) -> &Path {
        self
    }
}

impl MaybeMappedPath for PathBuf {
    fn real_path(&self) -> &Path {
        self
    }

    fn display_path(&self) -> &Path {
        self
    }
}
