use std::{
    ops::Not,
    path::{Path, PathBuf},
};

/// The local copies of the remote paths listed in `feature.fs.prefetch`.
#[derive(Debug, Default)]
pub struct PrefetchedFiles {
    /// Root of the local copies, mirroring the remote layout.
    ///
    /// [`None`] when nothing was prefetched.
    root: Option<PathBuf>,

    /// The remote paths that were asked for.
    ///
    /// Consulted before the filesystem is touched, so that an application reading unrelated files
    /// does not pay a `stat` on every open.
    prefetched: Vec<PathBuf>,
}

impl PrefetchedFiles {
    pub fn new(root: Option<PathBuf>, prefetched: &[String]) -> Self {
        Self {
            root,
            prefetched: prefetched.iter().map(PathBuf::from).collect(),
        }
    }

    /// Where the copy of `path` would live, had it been prefetched.
    fn copy_path(&self, path: &Path) -> Option<PathBuf> {
        let root = self.root.as_ref()?;

        if self
            .prefetched
            .iter()
            .any(|prefetched| path.starts_with(prefetched))
            .not()
        {
            return None;
        }

        Some(root.join(path.strip_prefix("/").ok()?))
    }

    /// The local copy of `path`, if there is one.
    pub fn local_copy(&self, path: &Path) -> Option<PathBuf> {
        let copy = self.copy_path(path)?;

        copy.exists().then_some(copy)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn prefetched() -> PrefetchedFiles {
        PrefetchedFiles::new(
            Some(PathBuf::from("/tmp/mirrord-prefetch-1")),
            &["/etc/ssl".to_owned(), "/app/config.yaml".to_owned()],
        )
    }

    #[rstest]
    #[case("/etc/ssl", Some("/tmp/mirrord-prefetch-1/etc/ssl"))]
    #[case(
        "/etc/ssl/certs/ca.pem",
        Some("/tmp/mirrord-prefetch-1/etc/ssl/certs/ca.pem")
    )]
    #[case("/app/config.yaml", Some("/tmp/mirrord-prefetch-1/app/config.yaml"))]
    #[case("/etc/passwd", None)]
    #[case("/app/config.yaml.bak", None)]
    // A sibling whose name merely starts with a prefetched path is not part of it.
    #[case("/etc/sslkeys/key.pem", None)]
    fn copy_path(#[case] path: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            prefetched().copy_path(Path::new(path)),
            expected.map(PathBuf::from)
        );
    }

    #[rstest]
    fn nothing_is_prefetched_without_a_root() {
        let prefetched = PrefetchedFiles::new(None, &["/etc/ssl".to_owned()]);

        assert_eq!(
            prefetched.copy_path(Path::new("/etc/ssl/certs/ca.pem")),
            None
        );
    }
}
