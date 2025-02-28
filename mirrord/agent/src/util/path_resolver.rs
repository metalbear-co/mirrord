use std::{
    io,
    path::{Component, Path, PathBuf},
};

use tracing::Level;

/// A helper struct for resolving paths as seen in the target container to paths accessible from the
/// root host.
///
/// Should be used whenever we need to access a file in the target container filesystem.
#[derive(Debug, Clone)]
pub struct InTargetPathResolver {
    root: PathBuf,
}

impl InTargetPathResolver {
    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn new(target_pid: u64) -> Self {
        let root = format!("/proc/{target_pid}/root");

        Self {
            root: PathBuf::from(root),
        }
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    pub fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        let mut temp_path = PathBuf::new();

        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Prefix(prefix) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("path prefix is not supported: {prefix:?}"),
                ))?,
                Component::CurDir => {}
                Component::ParentDir => {
                    if !temp_path.pop() {
                        tracing::warn!(?path, "Detected a possible LFI attempt",);

                        return Err(io::ErrorKind::NotFound.into());
                    }
                }
                Component::Normal(component) => {
                    let mut real_path = self.root.join(&temp_path);
                    real_path.push(component);

                    if real_path.is_symlink() {
                        let sym_dest = real_path.read_link()?;
                        temp_path = temp_path.join(sym_dest);
                    } else {
                        temp_path = temp_path.join(component);
                    }

                    if temp_path.has_root() {
                        temp_path = temp_path
                            .strip_prefix("/")
                            .map_err(|_| {
                                io::Error::new(io::ErrorKind::InvalidInput, "couldn't strip prefix")
                            })?
                            .into();
                    }
                }
            }
        }

        Ok(self.root.join(temp_path))
    }
}

#[cfg(test)]
impl InTargetPathResolver {
    /// Constructs a new resolver with the given root path.
    ///
    /// Makes it easy to test with [`tempfile::tempdir`].
    pub fn with_root_path(root: PathBuf) -> Self {
        Self { root }
    }
}
