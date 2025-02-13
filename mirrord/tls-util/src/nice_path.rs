use std::path::{Path, PathBuf};

pub trait NicePath {
    fn real_path(&self) -> &Path;

    fn display_path(&self) -> &Path;
}

impl NicePath for Path {
    fn real_path(&self) -> &Path {
        self
    }

    fn display_path(&self) -> &Path {
        self
    }
}

impl NicePath for PathBuf {
    fn real_path(&self) -> &Path {
        self
    }

    fn display_path(&self) -> &Path {
        self
    }
}
