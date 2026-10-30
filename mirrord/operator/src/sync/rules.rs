//! The gitignore rules of a push, which the CLI and the sync sidecar must apply alike.
//!
//! The CLI leaves the paths that git ignores out of a push, and a full push keeps them on the
//! volume, so the sidecar must ignore exactly the paths that the CLI ignored. The sidecar can't
//! read the rules from the volume, where a `.gitignore` may be stale or written by the app, so
//! the CLI sends an [`IgnoreSnapshot`] of every source of rules with each push, and both sides
//! evaluate it with [`IgnoreRules`].
//!
//! This module needs no feature of the crate, so that the static sidecar can use it without
//! `kube`.

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::{
    borrow::Cow,
    ffi::OsStr,
    ops::Not,
    path::{Component, Path, PathBuf},
};

use bincode::{Decode, Encode};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

#[cfg(test)]
mod tests;

/// The gitignore rules that apply to the tree of one entry of a push.
///
/// Git matches the patterns of a `.gitignore` relative to the directory that holds it, and those
/// of `.git/info/exclude` and of the global excludes file relative to the top of the working tree
/// ([gitignore(5)](https://git-scm.com/docs/gitignore#_pattern_format)), so a pattern such as
/// `/dist` in a repository's root `.gitignore` only matches correctly from the repository's root.
/// So every path in the snapshot is relative to the root of the entry's repository, as the bytes
/// of `/`-separated components, which keep a name that isn't UTF-8 as it is. Outside a
/// repository, the entry's local directory counts as the root.
#[derive(Encode, Decode, Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreSnapshot {
    /// The entry's local directory, relative to the root of its repository. Empty when the
    /// directory is the root, or lies outside any repository.
    pub prefix: Vec<u8>,

    /// The sources of rules, in git's order of precedence, lowest first: the global excludes
    /// file, `.git/info/exclude`, every `.gitignore` from the repository's root down, and last
    /// the `ignore` list of the entry in `mirrord-up.yaml`. A later source decides over an
    /// earlier one, so a `!` pattern in it can re-include a path.
    pub sources: Vec<IgnoreSource>,
}

/// One source of gitignore rules, such as a `.gitignore` file.
#[derive(Encode, Decode, Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreSource {
    /// The directory that the patterns are relative to, relative to the root of the repository.
    /// Empty for the root.
    pub root: Vec<u8>,

    /// The lines of the source, as in a `.gitignore` file.
    pub lines: Vec<String>,
}

/// The names of the directories of version control systems, which a push never carries and a
/// full push never deletes, whatever the rules say. Git can't re-include `.git` either.
pub const VCS_DIRECTORIES: [&str; 2] = [".git", ".jj"];

/// An [`IgnoreSnapshot`], ready to decide which paths of its entry are ignored.
///
/// The patterns of each source are matched below a virtual root, `/` followed by the source's
/// directory, so no path of the machine that evaluates the rules plays a part.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    snapshot: IgnoreSnapshot,

    /// The entry's directory under the virtual root.
    entry: PathBuf,

    /// Each source's directory under the virtual root, with its patterns, in the order of
    /// [`IgnoreSnapshot::sources`].
    sources: Vec<(PathBuf, Gitignore)>,

    /// How many of the sources at the end come from [`Self::add_last`], and stay after the ones
    /// that [`Self::add_gitignore`] adds.
    last_sources: usize,
}

impl IgnoreRules {
    pub fn new(snapshot: IgnoreSnapshot) -> Self {
        Self {
            entry: virtual_path(&snapshot.prefix),
            sources: snapshot.sources.iter().map(build).collect(),
            snapshot,
            last_sources: 0,
        }
    }

    /// The snapshot that the rules evaluate, with the sources that were added to it.
    pub fn snapshot(&self) -> &IgnoreSnapshot {
        &self.snapshot
    }

    /// Adds `lines`, the `.gitignore` of `directory`, a directory relative to the entry's
    /// directory, after the sources that the rules hold, but before the ones from
    /// [`Self::add_last`]. The CLI adds each `.gitignore` that it finds while it walks the tree,
    /// before it decides on the paths below it, as git does.
    pub fn add_gitignore(&mut self, directory: &Path, lines: Vec<String>) {
        let index = self.snapshot.sources.len() - self.last_sources;
        self.insert(index, directory, lines);
    }

    /// Adds `lines`, rooted at `directory`, a directory relative to the entry's directory, as the
    /// last source, which keeps the last word over every source that
    /// [`Self::add_gitignore`] adds later. The CLI adds the entry's own `ignore` list this way.
    pub fn add_last(&mut self, directory: &Path, lines: Vec<String>) {
        self.insert(self.snapshot.sources.len(), directory, lines);
        self.last_sources += 1;
    }

    /// Whether a source of the rules is rooted at `directory`, a path relative to the entry's
    /// directory, or below it, such as the `.gitignore` of `directory` or of one of its
    /// subdirectories.
    pub fn has_source_within(&self, directory: &Path) -> bool {
        let root = join_relative(&self.snapshot.prefix, directory);
        self.snapshot.sources.iter().any(|source| {
            source
                .root
                .strip_prefix(root.as_slice())
                .is_some_and(|rest| {
                    rest.is_empty() || rest.first() == Some(&b'/') || root.is_empty()
                })
        })
    }

    fn insert(&mut self, index: usize, directory: &Path, lines: Vec<String>) {
        let source = IgnoreSource {
            root: join_relative(&self.snapshot.prefix, directory),
            lines,
        };
        self.sources.insert(index, build(&source));
        self.snapshot.sources.insert(index, source);
    }

    /// Whether git would ignore `path`, a path relative to the entry's directory, which is a
    /// directory when `is_dir` is set.
    ///
    /// Follows git's algorithm: git checks every directory on the way down to a path, and
    /// doesn't look into an ignored one, so a path below an ignored directory is ignored
    /// whatever its own patterns say. At each level, the last matching pattern in the order of
    /// precedence decides. The entry's own directory, and the directories above it, are never
    /// ignored, because the developer chose to push the entry. A path with a component that
    /// isn't a plain name, such as `..`, is ignored, because no push carries it.
    ///
    /// A walk that only enters the directories that it keeps should use [`Self::directory`],
    /// which doesn't check the directories above each path again.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut directory = self.directory(Path::new(""));
        let mut components = path.components().peekable();

        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return true;
            };

            let component_is_dir = components.peek().is_some() || is_dir;
            if directory.is_ignored(name, component_is_dir) {
                return true;
            }

            if component_is_dir {
                directory.enter(name);
            }
        }

        false
    }

    /// The rules for the children of `directory`, a directory relative to the entry's directory
    /// that isn't ignored. Only the sources whose directory is `directory` or lies above it
    /// apply to its children, so the handle picks them once for all the children.
    pub fn directory(&self, directory: &Path) -> DirectoryRules<'_> {
        let mut rules = DirectoryRules {
            rules: self,
            path: self.entry.clone(),
            sources: Vec::new(),
        };
        rules.path.extend(directory);
        rules.pick_sources();
        rules
    }
}

/// The rules for the children of one directory, from [`IgnoreRules::directory`].
#[derive(Debug)]
pub struct DirectoryRules<'a> {
    rules: &'a IgnoreRules,

    /// The directory under the virtual root.
    path: PathBuf,

    /// The patterns of the sources that apply to the directory's children, in the order of
    /// precedence.
    sources: Vec<&'a Gitignore>,
}

impl DirectoryRules<'_> {
    /// Whether git would ignore the child `name` of the directory, which is a directory when
    /// `is_dir` is set. The last matching pattern in the order of precedence decides.
    pub fn is_ignored(&self, name: &OsStr, is_dir: bool) -> bool {
        if VCS_DIRECTORIES.iter().any(|vcs| name == *vcs) {
            return true;
        }

        let child = self.path.join(name);
        self.sources
            .iter()
            .rev()
            .map(|gitignore| gitignore.matched(&child, is_dir))
            .find(|matched| matched.is_none().not())
            .is_some_and(|matched| matched.is_ignore())
    }

    /// Moves the handle down to the child directory `name`.
    fn enter(&mut self, name: &OsStr) {
        self.path.push(name);
        self.pick_sources();
    }

    fn pick_sources(&mut self) {
        self.sources = self
            .rules
            .sources
            .iter()
            .filter(|(root, _)| self.path.starts_with(root))
            .map(|(_, gitignore)| gitignore)
            .collect();
    }
}

/// The patterns of `source`, rooted at its directory under the virtual root. A line that isn't a
/// valid pattern matches nothing, on both sides alike.
fn build(source: &IgnoreSource) -> (PathBuf, Gitignore) {
    let root = virtual_path(&source.root);
    let mut builder = GitignoreBuilder::new(&root);

    for line in &source.lines {
        let _ = builder.add_line(None, line);
    }

    let gitignore = builder.build().unwrap_or_else(|_| Gitignore::empty());
    (root, gitignore)
}

/// `relative`, the bytes of a `/`-separated path relative to the root of a repository, under the
/// virtual root.
fn virtual_path(relative: &[u8]) -> PathBuf {
    let mut path = PathBuf::from("/");
    path.extend(
        relative
            .split(|byte| *byte == b'/')
            .filter(|component| component.is_empty().not())
            .map(name_from_bytes),
    );
    path
}

/// `relative` appended to `prefix`, as the bytes of `/`-separated components, the form of the
/// paths of an [`IgnoreSnapshot`].
pub(crate) fn join_relative(prefix: &[u8], relative: &Path) -> Vec<u8> {
    let components = prefix
        .split(|byte| *byte == b'/')
        .map(Cow::Borrowed)
        .chain(
            relative
                .components()
                .map(|component| name_bytes(component.as_os_str())),
        )
        .filter(|component| component.is_empty().not())
        .collect::<Vec<_>>();
    components.join(&b'/')
}

/// The bytes of the file name `name`. Outside Unix, where a name needn't be bytes, a name that
/// isn't Unicode loses what it can't keep.
fn name_bytes(name: &OsStr) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    return Cow::Borrowed(name.as_bytes());

    #[cfg(not(unix))]
    match name.to_string_lossy() {
        Cow::Borrowed(name) => Cow::Borrowed(name.as_bytes()),
        Cow::Owned(name) => Cow::Owned(name.into_bytes()),
    }
}

/// The file name whose bytes are `bytes`, the reverse of [`name_bytes`].
fn name_from_bytes(bytes: &[u8]) -> Cow<'_, OsStr> {
    #[cfg(unix)]
    return Cow::Borrowed(OsStr::from_bytes(bytes));

    #[cfg(not(unix))]
    match String::from_utf8_lossy(bytes) {
        Cow::Borrowed(name) => Cow::Borrowed(OsStr::new(name)),
        Cow::Owned(name) => Cow::Owned(name.into()),
    }
}
