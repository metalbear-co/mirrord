use std::{
    io,
    ops::Not,
    os::unix::ffi::OsStrExt,
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
    /// Number of chained symlinks that we can resolve before returning ELOOP.
    /// 40 matches the default on Linux.
    const MAX_SYMLINK_HOPS: u8 = 40;

    pub fn from_pid(target_pid: u64) -> Self {
        let root = format!("/proc/{target_pid}/root");

        Self {
            root: PathBuf::from(root),
        }
    }

    pub fn from_root() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    /// Returns the given path, resolved with [`Self::root`] as root.
    /// The returned path will never climb above [`Self::root`] and
    /// will contain no symlinks.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::DEBUG))]
    pub fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        let mut depth = Self::MAX_SYMLINK_HOPS;
        self.resolve_inner(path, PathBuf::new(), &mut depth)
            .map(|p| self.root.join(&p))
    }

    /// Main (bounded) recursive implementation function for resolving paths.
    /// Returns paths *without* the [`Self::root`] prefix, so these paths are
    /// *relative* to [`Self::root`]. Use [`Self::resolve`] to get real paths.
    /// This function is mainly for normalizing the path and correctly resolving
    /// any symlinks.
    ///
    /// `max_depth` is the max number of symlinks we are allowed to resolve
    /// before returning ELOOP. We use a `&mut` instead of passing it by value
    /// because it is decremented in recursive child calls, of which there might
    /// be multiple at the same recursion depth.
    fn resolve_inner(&self, path: &Path, from: PathBuf, max_depth: &mut u8) -> io::Result<PathBuf> {
        let mut tmp_path = if path.has_root() {
            PathBuf::new()
        } else {
            from
        };

        for comp in path.components() {
            match comp {
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "path prefixes are not supported",
                    ));
                }
                Component::RootDir => {}
                Component::CurDir => {}
                Component::ParentDir => {
                    tmp_path.pop();
                }
                Component::Normal(comp) => {
                    tmp_path.push(comp);
                    let real_path = self.root.join(&tmp_path);

                    // We don't use [`Path::is_symlink`] because it
                    // consumes all errors and returns false.
                    let is_symlink = match real_path.symlink_metadata() {
                        Ok(meta) => meta.file_type().is_symlink(),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
                        Err(err) => return Err(err),
                    };

                    if is_symlink.not() {
                        continue;
                    }

                    if *max_depth == 0 {
                        return Err(io::Error::from_raw_os_error(libc::ELOOP));
                    }

                    *max_depth -= 1;

                    // Symlink logic
                    let link = real_path.read_link()?;

                    let from = tmp_path
                        .parent()
                        .expect("tmp_path should not be empty at this point");

                    tmp_path = self.resolve_inner(&link, from.to_path_buf(), max_depth)?;
                }
            }
        }

        assert!(tmp_path.has_root().not());

        if path.as_os_str().as_bytes().ends_with(b"/") {
            tmp_path.push("");
        }

        Ok(tmp_path)
    }

    /// Resolves `path` like [`Self::resolve`], but does not follow a symlink in the last
    /// component.
    ///
    /// Meant for operations that act on the link itself, such as `lstat` and `readlink`. Joining
    /// the unresolved path onto the target root is not enough for those: the kernel resolves
    /// absolute symlinks in the intermediate components (e.g. `/var/run -> /run`) against the
    /// agent's root instead of the target's.
    ///
    /// A trailing slash makes the last component follow symlinks, same as in the kernel.
    pub fn resolve_no_follow(&self, path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().as_bytes().ends_with(b"/") {
            return self.resolve(path);
        }

        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => Ok(self.resolve(parent)?.join(name)),
            _ => self.resolve(path),
        }
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    /// Target root with `/var/run -> /run` (absolute) and `/run/secrets/token -> ..data/token`.
    fn target_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("var")).unwrap();
        fs::create_dir_all(root.path().join("run/secrets/..data")).unwrap();
        fs::write(root.path().join("run/secrets/..data/token"), "secret").unwrap();
        symlink("/run", root.path().join("var/run")).unwrap();
        symlink("..data/token", root.path().join("run/secrets/token")).unwrap();
        root
    }

    #[test]
    fn no_follow_resolves_absolute_symlink_in_parent() {
        let root = target_root();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver
            .resolve_no_follow(Path::new("/var/run/secrets/token"))
            .unwrap();

        assert_eq!(resolved, root.path().join("run/secrets/token"));
        assert!(resolved.symlink_metadata().unwrap().is_symlink());
        assert_eq!(resolved.read_link().unwrap(), PathBuf::from("..data/token"));
    }

    #[test]
    fn no_follow_keeps_last_component_symlink() {
        let root = target_root();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve_no_follow(Path::new("/var/run")).unwrap();

        assert_eq!(resolved, root.path().join("var/run"));
        assert_eq!(resolved.read_link().unwrap(), PathBuf::from("/run"));
    }

    #[test]
    fn no_follow_with_trailing_slash_follows_last_component() {
        let root = target_root();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve_no_follow(Path::new("/var/run/")).unwrap();

        assert!(resolved.symlink_metadata().unwrap().is_dir());
    }

    #[test]
    fn no_follow_root() {
        let root = target_root();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve_no_follow(Path::new("/")).unwrap();

        assert!(resolved.symlink_metadata().unwrap().is_dir());
    }

    /// `/a/b -> c`, `/a/c -> d`, `/a/d` is a file.
    #[test]
    fn follows_chain_of_relative_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("a")).unwrap();
        fs::write(root.path().join("a/d"), "").unwrap();
        symlink("c", root.path().join("a/b")).unwrap();
        symlink("d", root.path().join("a/c")).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/a/b")).unwrap();

        assert_eq!(resolved, root.path().join("a/d"));
    }

    /// `/x -> /y`, `/y -> /z`, `/z` is a file.
    #[test]
    fn follows_chain_of_absolute_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("z"), "").unwrap();
        symlink("/y", root.path().join("x")).unwrap();
        symlink("/z", root.path().join("y")).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/x")).unwrap();

        assert_eq!(resolved, root.path().join("z"));
    }

    /// `/a -> b`, `/b -> a`.
    #[test]
    fn symlink_loop_is_eloop() {
        let root = tempfile::tempdir().unwrap();
        symlink("b", root.path().join("a")).unwrap();
        symlink("a", root.path().join("b")).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let err = resolver.resolve(Path::new("/a")).unwrap_err();

        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
    }

    /// `/d0 -> d1/../d1`, `/d1 -> d2/../d2`, and so on down to a real directory.
    ///
    /// Naming the next link twice makes every level traverse it twice, so resolving `/d0` costs
    /// `2^CHAIN` traversals. The hop budget therefore has to be shared across the whole
    /// resolution: a budget that only bounds nesting depth accepts this path and spends
    /// exponential time on it, since the depth is merely `CHAIN`.
    ///
    /// `CHAIN` is kept small so that a regression fails this assertion in milliseconds instead
    /// of hanging the suite.
    #[test]
    fn branching_symlink_chain_exhausts_shared_hop_budget() {
        const CHAIN: usize = 12;

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        for i in 0..CHAIN - 1 {
            symlink(
                format!("d{}/../d{}", i + 1, i + 1),
                root.path().join(format!("d{i}")),
            )
            .unwrap();
        }
        symlink("dir", root.path().join(format!("d{}", CHAIN - 1))).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let err = resolver.resolve(Path::new("/d0")).unwrap_err();

        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
    }

    /// `/x -> /var/run/secrets/token`, where `/var/run -> /run` is absolute. The destination's
    /// intermediate components must be resolved against the target root too, otherwise the
    /// kernel resolves `/var/run` against the agent's root when the returned path is opened.
    #[test]
    fn follows_absolute_symlink_inside_destination() {
        let root = target_root();
        symlink("/var/run/secrets/token", root.path().join("x")).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/x")).unwrap();

        assert_eq!(resolved, root.path().join("run/secrets/..data/token"));
    }

    /// `/x -> ../outside`, where `outside` sits next to the target root on the agent's
    /// filesystem. A `..` in a destination clamps at the root instead of reaching the sibling.
    #[test]
    fn parent_dir_in_destination_cannot_reach_agent_sibling() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(parent.path().join("outside"), "").unwrap();
        symlink("../outside", root.join("x")).unwrap();
        let resolver = InTargetPathResolver::with_root_path(root.clone());

        let resolved = resolver.resolve(Path::new("/x")).unwrap();

        assert_eq!(resolved, root.join("outside"));
        assert!(resolved.symlink_metadata().is_err());
    }

    /// Target root with a set of interlinked symlinks:
    ///
    /// ```text
    /// dir/                      directory
    /// dir/file                  regular file
    /// dir/sibling            -> file
    /// dir/up_rel             -> ../rel_file
    /// dir/up_abs             -> ../abs_dir/file
    /// dir/two_hop            -> up_abs
    /// dir/agent_only         -> /dev/null
    /// abs_dir                -> /dir
    /// rel_file               -> dir/file
    /// rel_to_abs             -> abs_dir
    /// escape                 -> ../../..
    /// ```
    fn symlink_maze() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        fs::write(root.path().join("dir/file"), "").unwrap();
        symlink("file", root.path().join("dir/sibling")).unwrap();
        symlink("../rel_file", root.path().join("dir/up_rel")).unwrap();
        symlink("../abs_dir/file", root.path().join("dir/up_abs")).unwrap();
        symlink("up_abs", root.path().join("dir/two_hop")).unwrap();
        symlink("/dev/null", root.path().join("dir/agent_only")).unwrap();
        symlink("/dir", root.path().join("abs_dir")).unwrap();
        symlink("dir/file", root.path().join("rel_file")).unwrap();
        symlink("abs_dir", root.path().join("rel_to_abs")).unwrap();
        symlink("../../..", root.path().join("escape")).unwrap();
        root
    }

    /// `/dir/sibling -> file` resolves against the directory holding the link, not the root.
    #[test]
    fn relative_destination_resolves_against_links_own_directory() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/dir/sibling")).unwrap();

        assert_eq!(resolved, root.path().join("dir/file"));
    }

    /// `/rel_to_abs -> abs_dir`, where `abs_dir -> /dir` is absolute. A chain may switch from a
    /// relative destination to an absolute one partway through.
    #[test]
    fn relative_destination_chains_into_absolute_symlink() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/rel_to_abs")).unwrap();

        assert_eq!(resolved, root.path().join("dir"));
    }

    /// `/dir/up_rel -> ../rel_file`, where `rel_file -> dir/file`. A `..` inside a destination
    /// applies to the link's own directory.
    #[test]
    fn parent_dir_inside_destination_is_resolved() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/dir/up_rel")).unwrap();

        assert_eq!(resolved, root.path().join("dir/file"));
    }

    /// `/dir/two_hop -> up_abs -> ../abs_dir/file`, where `abs_dir -> /dir`. The second hop
    /// combines a `..` with an absolute symlink in an intermediate component.
    #[test]
    fn chains_through_parent_dir_and_absolute_symlink() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/dir/two_hop")).unwrap();

        assert_eq!(resolved, root.path().join("dir/file"));
    }

    /// `/dir/agent_only -> /dev/null`. The destination exists on the agent's filesystem but not
    /// in the target, so resolution must yield a missing path under the target root rather than
    /// the agent's own device node.
    #[test]
    fn absolute_destination_never_escapes_to_agent_root() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/dir/agent_only")).unwrap();

        assert_eq!(resolved, root.path().join("dev/null"));
        assert!(resolved.symlink_metadata().is_err());
    }

    /// `/escape -> ../../..` climbs past the target root, which clamps to the root itself.
    #[test]
    fn parent_dir_beyond_root_clamps_to_root() {
        let root = symlink_maze();
        let resolver = InTargetPathResolver::with_root_path(root.path().to_path_buf());

        let resolved = resolver.resolve(Path::new("/escape")).unwrap();

        assert_eq!(resolved, root.path().to_path_buf());
    }
}
