use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(unix)]
use std::{
    ffi::OsStr,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
};

use flate2::read::GzDecoder;
use futures::{SinkExt, StreamExt};
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{
    PackedEntry, PreviewSyncConnection, PreviewSyncError, RuleSources, SyncEvent, TreeListing,
    pack_changes, pack_paths,
};
use crate::sync::{
    EntryCounts, EntryPlan, MAX_DATA_CHUNK_BYTES, ManifestKind, ManifestPath, PushMode,
    RejectReason, SYNC_PROTOCOL_VERSION, SidecarState, SyncFrame, UnpairReason,
    rules::{IgnoreRules, IgnoreSnapshot, IgnoreSource},
};

/// How long a test waits for a push before it fails, so that a push that stalls fails the test
/// rather than hang it.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What an archive holds at one path.
#[derive(Debug, PartialEq, Eq)]
enum Packed {
    Directory,

    /// A file with its contents and whether its owner may execute it.
    File(String, bool),

    /// A symlink with its target.
    Symlink(String),
}

/// Lists the tree under `local` with `ignore` as the entry's own gitignore lines, as the CLI
/// lists it without a watcher.
fn list(local: &Path, ignore: &[String]) -> io::Result<TreeListing> {
    TreeListing::new_watching(&RuleSources::new(local)?, ignore, &mut |_| Ok(()))
}

/// The whole listed tree under `local` as entry `index` of a push.
fn pack_listing(listing: &TreeListing, index: u32, local: &Path) -> io::Result<PackedEntry> {
    pack_paths(
        index,
        local,
        &listing.paths,
        listing.rules.snapshot(),
        Vec::new(),
    )
}

/// The entry that a push of `local` carries, with `ignore` as the entry's own gitignore lines.
fn pack(local: &Path, ignore: &[&str]) -> PackedEntry {
    let ignore = ignore
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    pack_listing(&list(local, &ignore).unwrap(), 0, local).unwrap()
}

/// What the archive of `entry` holds, by path.
fn unpacked(entry: &PackedEntry) -> BTreeMap<String, Packed> {
    let mut archive = tar::Archive::new(GzDecoder::new(entry.archive.as_slice()));
    let mut packed = BTreeMap::new();

    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().display().to_string();
        let header = entry.header();

        let item = if header.entry_type().is_dir() {
            Packed::Directory
        } else if header.entry_type().is_symlink() {
            Packed::Symlink(entry.link_name().unwrap().unwrap().display().to_string())
        } else {
            let executable = header.mode().unwrap() & 0o100 != 0;
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            Packed::File(contents, executable)
        };

        packed.insert(path.trim_end_matches('/').to_owned(), item);
    }

    packed
}

/// A push must carry the tree that the developer works on: what git ignores stays on their
/// machine, and what the dev server needs, such as executable scripts and symlinks, keeps its
/// kind.
#[cfg(unix)]
#[test]
fn push_leaves_out_what_git_ignores() {
    let local = tempfile::tempdir().unwrap();
    let write = |path: &str, contents: &str| {
        let path = local.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(".gitignore", "dist/\n*.log\n");
    write("app.js", "app");
    write("run.sh", "#!/bin/sh");
    fs::set_permissions(
        local.path().join("run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write("lib/util.js", "util");
    write("lib/.gitignore", "generated.js\n");
    write("lib/generated.js", "generated");
    write("dist/bundle.js", "bundle");
    write("debug.log", "log");
    // The push leaves out the directories of version control systems.
    write(".git/HEAD", "ref: refs/heads/main");
    write(".jj/.gitignore", "/*\n");
    write(".jj/repo/store", "store");
    symlink("app.js", local.path().join("link")).unwrap();

    let file = |contents: &str, executable| Packed::File(contents.to_owned(), executable);
    assert_eq!(
        unpacked(&pack(local.path(), &[])),
        BTreeMap::from([
            (".gitignore".to_owned(), file("dist/\n*.log\n", false)),
            ("app.js".to_owned(), file("app", false)),
            ("lib".to_owned(), Packed::Directory),
            ("lib/.gitignore".to_owned(), file("generated.js\n", false)),
            ("lib/util.js".to_owned(), file("util", false)),
            ("link".to_owned(), Packed::Symlink("app.js".to_owned())),
            ("run.sh".to_owned(), file("#!/bin/sh", true)),
        ])
    );
}

/// A service's directory is often a subdirectory of its repository, so its push must leave out
/// what the repository's root `.gitignore` and `.git/info/exclude` exclude. A `.gitignore` above
/// the repository doesn't count, as in git, because it can belong to something else, such as a
/// home directory that ignores everything.
#[test]
fn push_of_a_subdirectory_leaves_out_what_its_repository_ignores() {
    let home = tempfile::tempdir().unwrap();
    let write = |path: &str, contents: &str| {
        let path = home.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(".gitignore", "*\n");
    write("repo/.git/HEAD", "ref: refs/heads/main");
    write("repo/.git/info/exclude", "notes.txt\n");
    write("repo/.gitignore", "node_modules/\n.env\n");
    write("repo/frontend/app.js", "app");
    write("repo/frontend/.env", "SECRET=1");
    write("repo/frontend/notes.txt", "notes");
    write("repo/frontend/node_modules/left-pad/index.js", "dependency");

    let entry = pack(&home.path().join("repo/frontend"), &[]);
    assert_eq!(
        unpacked(&entry),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );

    // The sidecar keeps on the volume what the rules of the push ignore, so they must ignore
    // what the push left out, with the patterns of the repository's root.
    let rules = IgnoreRules::new(entry.plan.rules.clone());
    for (left_out, is_dir) in [
        (".env", false),
        ("notes.txt", false),
        ("node_modules", true),
    ] {
        assert!(rules.is_ignored(Path::new(left_out), is_dir), "{left_out}");
    }
    assert!(!rules.is_ignored(Path::new("app.js"), false));
}

/// A repository can name its own excludes file in its configuration, which takes precedence over
/// the developer's global one, and a push must leave out what that file excludes, which may be a
/// secret, however git's syntax writes the setting.
#[cfg(unix)]
#[test]
// Git is an `.exe` on Windows, which `Command::new` finds.
#[allow(clippy::disallowed_methods)]
fn push_leaves_out_what_the_repository_configuration_excludes() {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            // Keeps the developer's own configuration out of the comparison.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        output.status.code()
    };

    assert_eq!(git(&["init", "--quiet"]), Some(0));
    let excludes = repo.path().join("private-excludes");
    fs::write(&excludes, "private-key.pem\n").unwrap();
    assert_eq!(
        git(&["config", "core.excludesFile", excludes.to_str().unwrap()]),
        Some(0)
    );
    fs::write(repo.path().join("app.js"), "app").unwrap();
    fs::write(repo.path().join("private-key.pem"), "secret").unwrap();

    assert_eq!(
        git(&["check-ignore", "--no-index", "--quiet", "private-key.pem"]),
        Some(0)
    );
    let paths = unpacked(&pack(repo.path(), &[]))
        .into_keys()
        .collect::<Vec<_>>();
    assert_eq!(paths, ["app.js", "private-excludes"]);

    // A later `[core]` section wins, with an inline comment, with a quoted path followed by a
    // comment, with a path relative to the working tree, and in a file that the configuration
    // includes.
    let config = repo.path().join(".git/config");
    let base = fs::read_to_string(&config).unwrap();
    let included = repo.path().join(".git/included");
    fs::write(
        &included,
        format!("[core]\n\texcludesFile = {}\n", excludes.display()),
    )
    .unwrap();
    for section in [
        format!("[core]\n\texcludesFile = {} # the keys", excludes.display()),
        format!(
            "[core]\n\texcludesFile = \"{}\" ; the keys",
            excludes.display()
        ),
        "[core]\n\texcludesFile = private-excludes".to_owned(),
        format!("[include]\n\tpath = {}", included.display()),
    ] {
        fs::write(&config, format!("{base}{section}\n")).unwrap();

        assert_eq!(
            git(&["check-ignore", "--no-index", "--quiet", "private-key.pem"]),
            Some(0),
            "{section}"
        );
        let paths = unpacked(&pack(repo.path(), &[]))
            .into_keys()
            .collect::<Vec<_>>();
        assert_eq!(paths, ["app.js", "private-excludes"], "{section}");
    }
}

/// A `.gitignore` in a directory whose name isn't UTF-8 applies to that directory's files, as in
/// git.
#[cfg(unix)]
#[test]
fn push_leaves_out_what_a_gitignore_in_a_directory_with_a_non_utf8_name_excludes() {
    let local = tempfile::tempdir().unwrap();
    let directory = local.path().join(OsStr::from_bytes(b"caf\xe9"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join(".gitignore"), "private-key.pem\n").unwrap();
    fs::write(directory.join("app.js"), "app").unwrap();
    fs::write(directory.join("private-key.pem"), "secret").unwrap();

    let paths = unpacked(&pack(local.path(), &[]))
        .into_keys()
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(
        !paths.iter().any(|path| path.ends_with("private-key.pem")),
        "{paths:?}"
    );
}

/// A file that fails to pack in a large tree must be named, so that the developer can find it.
#[test]
fn a_file_that_fails_to_pack_is_named() {
    let local = tempfile::tempdir().unwrap();
    fs::create_dir(local.path().join("src")).unwrap();
    fs::write(local.path().join("src/app.js"), "app").unwrap();
    let listing = list(local.path(), &[]).unwrap();

    fs::remove_file(local.path().join("src/app.js")).unwrap();
    let error = pack_listing(&listing, 0, local.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("`{}`", local.path().join("src/app.js").display())),
        "{error}"
    );
}

/// The entry's own `ignore` lines leave out more, and re-include what a `.gitignore` excludes,
/// but never a file below an ignored directory. A `.gitignore` in the tree counts outside a
/// repository too.
#[test]
fn push_leaves_out_what_the_entry_ignores() {
    let local = tempfile::tempdir().unwrap();
    let write = |path: &str| {
        let path = local.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    };

    fs::write(local.path().join(".gitignore"), ".env*\ndist/\n").unwrap();
    write("app.js");
    write(".env");
    write(".env.preview");
    write("coverage/index.html");
    write("dist/.env.preview");

    let paths = unpacked(&pack(
        local.path(),
        &["coverage/", "!.env.preview", "!dist/.env.preview"],
    ))
    .into_keys()
    .collect::<Vec<_>>();
    assert_eq!(paths, [".env.preview", ".gitignore", "app.js"]);
}

/// The sidecar refuses a stream that holds more than `Begin` declared, so the declared counts
/// must be those of the stream.
#[cfg(unix)]
#[test]
fn push_declares_what_its_stream_holds() {
    let local = tempfile::tempdir().unwrap();
    fs::create_dir(local.path().join("lib")).unwrap();
    fs::write(local.path().join("lib/util.js"), "util").unwrap();
    fs::write(local.path().join("app.js"), "app, longer").unwrap();
    symlink("app.js", local.path().join("link")).unwrap();

    let entry = pack(local.path(), &[]);
    let mut archive = tar::Archive::new(GzDecoder::new(entry.archive.as_slice()));
    let (mut files, mut directories, mut bytes) = (0, 0, 0);
    for archived in archive.entries().unwrap() {
        let header = archived.unwrap().header().clone();
        if header.entry_type().is_dir() {
            directories += 1;
        } else {
            files += 1;
            bytes += header.size().unwrap();
        }
    }

    assert_eq!(
        (entry.plan.files, entry.plan.directories, entry.plan.bytes),
        (files, directories, bytes)
    );
    assert_eq!((files, directories, bytes), (3, 1, 15));
}

/// A dev server's socket in the tree must not stop the push of the files around it.
#[cfg(unix)]
#[test]
fn push_skips_sockets() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("app.js"), "app").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(local.path().join("dev.sock")).unwrap();

    assert_eq!(
        unpacked(&pack(local.path(), &[])),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );
}

/// The operator's end of a [`PreviewSyncConnection`], which a test scripts.
struct Operator(WebSocketStream<DuplexStream>);

impl Operator {
    async fn send(&mut self, frame: SyncFrame) {
        self.0
            .send(Message::Binary(frame.encode().into()))
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> SyncFrame {
        match self.0.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => SyncFrame::decode(&bytes).unwrap(),
            message => panic!("the CLI sent {message:?}"),
        }
    }

    /// Receives a full push and returns its id and its entries, each with the plan that `Begin`
    /// declared and what its `Data` frames carried, in the order they came. Checks that each
    /// entry's data ends with `EntryDone`.
    async fn recv_push(&mut self) -> (u64, Vec<PackedEntry>) {
        let SyncFrame::Begin {
            push_id,
            mode: PushMode::Full,
            entries: plans,
        } = self.recv().await
        else {
            panic!("a push starts with `Begin` of a full push");
        };

        let mut received = Vec::<PackedEntry>::new();
        let mut open = None;

        loop {
            match self.recv().await {
                SyncFrame::Data { index, chunk } => {
                    assert!(chunk.len() <= MAX_DATA_CHUNK_BYTES);

                    if open != Some(index) {
                        assert_eq!(open, None, "entry {index} began before the last one ended");
                        let plan = plans
                            .iter()
                            .find(|plan| plan.index == index)
                            .unwrap_or_else(|| panic!("entry {index} isn't in the push"));
                        received.push(PackedEntry {
                            plan: plan.clone(),
                            archive: Vec::new(),
                            deleted: Vec::new(),
                        });
                        open = Some(index);
                    }

                    received.last_mut().unwrap().archive.extend(chunk);
                }
                SyncFrame::EntryDone { index, deleted } => {
                    assert_eq!(open.take(), Some(index));
                    assert!(deleted.is_empty(), "a full push lists no deleted paths");
                }
                SyncFrame::Commit { push_id: committed } => {
                    assert_eq!((committed, open), (push_id, None));
                    return (push_id, received);
                }
                frame => panic!("unexpected frame in a push: {frame:?}"),
            }
        }
    }
}

/// A connected [`PreviewSyncConnection`] and the operator's end of it.
async fn connection() -> (PreviewSyncConnection<DuplexStream>, Operator) {
    let (cli, operator) = tokio::io::duplex(4 * MAX_DATA_CHUNK_BYTES);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    (PreviewSyncConnection::new(cli), Operator(operator))
}

/// The CLI reports the sidecar's state until the operator pairs it, then pushes each archive
/// whole, in chunks that the operator can relay, and takes the generation from the sidecar's
/// answer to its push.
#[tokio::test]
async fn push_reaches_the_sidecar_whole_after_pairing() {
    let (mut connection, mut operator) = connection().await;
    let entry = |index, archive| PackedEntry {
        plan: EntryPlan {
            index,
            files: 1,
            directories: 0,
            bytes: 10,
            rules: IgnoreSnapshot::default(),
        },
        archive,
        deleted: Vec::new(),
    };
    let large: Vec<u8> = (0..2 * MAX_DATA_CHUNK_BYTES + 1)
        .map(|byte| byte as u8)
        .collect();
    let entries = vec![entry(0, large), entry(2, b"small".to_vec())];

    let cli = async {
        let mut states = Vec::new();
        let paired = loop {
            match connection.next_event().await.unwrap() {
                SyncEvent::Paired { generation } => break generation,
                event => states.push(event),
            }
        };

        let generation = connection
            .push(5, PushMode::Full, entries.clone(), |_| {})
            .await
            .unwrap();

        (states, paired, generation)
    };

    let sidecar = async {
        operator
            .send(SyncFrame::Status(SidecarState::Disconnected))
            .await;
        operator
            .send(SyncFrame::Status(SidecarState::AwaitingFirstPush))
            .await;
        operator
            .send(SyncFrame::Paired {
                generation: None,
                protocol_version: SYNC_PROTOCOL_VERSION,
            })
            .await;

        let pushed = operator.recv_push().await;

        // The state can change while the sidecar applies the push.
        operator
            .send(SyncFrame::Status(SidecarState::Synced { generation: 1 }))
            .await;
        operator
            .send(SyncFrame::Applied {
                push_id: pushed.0,
                generation: 1,
                entries: Vec::new(),
            })
            .await;

        pushed
    };

    let ((states, paired, generation), pushed) =
        tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(cli, sidecar) })
            .await
            .expect("the push didn't finish");

    assert_eq!(
        states,
        [
            SyncEvent::Sidecar(SidecarState::Disconnected),
            SyncEvent::Sidecar(SidecarState::AwaitingFirstPush)
        ]
    );
    assert_eq!(paired, None);
    assert_eq!(generation, 1);
    assert_eq!(pushed, (5, entries));
}

/// A push that doesn't land must fail with the reason, so that `mirrord preview sync` can tell the
/// user and delete the session, rather than wait or report a generation that the pod doesn't
/// hold.
#[tokio::test]
async fn failed_push_reports_why() {
    async fn push_answered_with(answer: Option<SyncFrame>) -> PreviewSyncError {
        let (mut connection, mut operator) = connection().await;
        let entries = [PackedEntry {
            plan: EntryPlan {
                index: 0,
                files: 1,
                directories: 0,
                bytes: 7,
                rules: IgnoreSnapshot::default(),
            },
            archive: b"archive".to_vec(),
            deleted: Vec::new(),
        }];
        let cli = connection.push(5, PushMode::Full, entries.to_vec(), |_| {});

        let sidecar = async {
            operator.recv_push().await;

            match answer {
                Some(answer) => operator.send(answer).await,
                None => operator.0.close(None).await.unwrap(),
            }

            operator
        };

        let (result, _operator) =
            tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(cli, sidecar) })
                .await
                .expect("the push didn't finish");
        result.unwrap_err()
    }

    let rejected = push_answered_with(Some(SyncFrame::Rejected {
        push_id: 5,
        reason: RejectReason::Invalid("`../evil.js` leaves the entry".to_owned()),
    }))
    .await;
    assert!(
        matches!(&rejected, PreviewSyncError::Rejected(RejectReason::Invalid(reason)) if reason.contains("leaves the entry")),
        "{rejected}"
    );

    let stopped = push_answered_with(Some(SyncFrame::Error {
        message: "the preview pod's sync sidecar disconnected".to_owned(),
    }))
    .await;
    assert!(
        matches!(&stopped, PreviewSyncError::Operator(message) if message.contains("disconnected")),
        "{stopped}"
    );

    let closed = push_answered_with(None).await;
    assert!(matches!(closed, PreviewSyncError::Closed), "{closed}");

    // An answer to another push, for example one that an earlier CLI sent, is not this push's.
    let other = push_answered_with(Some(SyncFrame::Applied {
        push_id: 4,
        generation: 9,
        entries: vec![EntryCounts {
            index: 0,
            written: 1,
            unchanged: 0,
            deleted: 0,
        }],
    }))
    .await;
    assert!(matches!(other, PreviewSyncError::Unexpected(_)), "{other}");
}

/// jj honours the `.gitignore` files of its repository also when it keeps its git data outside
/// the working tree, so a `local` in such a repository gets the `.gitignore` of the directories
/// above it, as in a git repository.
#[test]
fn a_jj_repository_without_git_applies_the_gitignore_files_above_local() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join(".jj")).unwrap();
    fs::write(repo.path().join(".gitignore"), "*.log\n").unwrap();
    let local = repo.path().join("service");
    fs::create_dir(&local).unwrap();
    fs::write(local.join("app.js"), "app").unwrap();
    fs::write(local.join("debug.log"), "log").unwrap();

    let paths = unpacked(&pack(&local, &[])).into_keys().collect::<Vec<_>>();
    assert_eq!(paths, ["app.js"]);
}

/// The incremental push of the paths in `changed` of the tree under `local`, with the rules of
/// the tree's listing, or `None` when the push must be a full one.
fn pack_changed(local: &Path, ignore: &[&str], changed: &[&str]) -> Option<PackedEntry> {
    let ignore = ignore
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    let listing = list(local, &ignore).unwrap();
    let rules = listing.rules().clone();
    let changed = changed.iter().map(PathBuf::from).collect();

    pack_changes(0, local, &rules, &changed, &mut |_| Ok(())).unwrap()
}

/// Build tools write a `.gitignore` into their output directory, which the rules ignore, and git
/// never reads a `.gitignore` in an ignored directory, so each build must not force a full push.
#[test]
fn a_gitignore_in_an_ignored_directory_changes_no_rule() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join(".gitignore"), "dist/\n").unwrap();
    fs::create_dir(local.path().join("dist")).unwrap();
    fs::write(local.path().join("dist/.gitignore"), "*\n").unwrap();

    let packed = pack_changed(local.path(), &[], &["dist/.gitignore"]);

    assert!(packed.is_some_and(|packed| unpacked(&packed).is_empty()));
}

/// Git reads the `.gitignore` of a directory even when the rules ignore that file, so a new
/// directory whose `.gitignore` the entry's `ignore` list ignores still changes the rules, and
/// the next incremental push would otherwise apply rules without it.
#[test]
fn an_ignored_gitignore_in_a_new_directory_changes_the_rules() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("app.js"), "app").unwrap();
    fs::create_dir(local.path().join("new")).unwrap();
    fs::write(local.path().join("new/.gitignore"), "secret\n").unwrap();
    fs::write(local.path().join("new/secret"), "secret").unwrap();

    assert_eq!(pack_changed(local.path(), &[".gitignore"], &["new"]), None);
}

/// Deleting a `.gitignore`, or changing one that the entry's `ignore` list matches, changes the
/// rules as much as a new one does. Git reads a `.gitignore` also when a rule matches the file
/// itself, so an incremental push would otherwise keep a stale rule, and send a file that the
/// changed rules leave out.
#[test]
fn a_deleted_or_ignored_gitignore_changes_the_rules() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("app.js"), "app").unwrap();

    assert_eq!(pack_changed(local.path(), &[], &[".gitignore"]), None);

    fs::write(local.path().join(".gitignore"), ".env\n").unwrap();
    fs::write(local.path().join(".env"), "private").unwrap();
    assert_eq!(
        pack_changed(local.path(), &[".gitignore"], &[".gitignore", ".env"]),
        None
    );
}

/// Replacing a directory whole, as a branch switch or a build tool does, takes its `.gitignore`
/// away without an event that names the file, so a change to a directory that held a source of
/// the rules must change the rules too, or `src/app.js` would stay ignored.
#[test]
fn replacing_a_directory_that_holds_a_gitignore_changes_the_rules() {
    let local = tempfile::tempdir().unwrap();
    fs::create_dir(local.path().join("src")).unwrap();
    fs::write(local.path().join("src/.gitignore"), "app.js\n").unwrap();
    fs::write(local.path().join("src/app.js"), "app").unwrap();
    let listing = list(local.path(), &[]).unwrap();
    let rules = listing.rules().clone();

    let elsewhere = tempfile::tempdir_in(local.path().parent().unwrap()).unwrap();
    fs::rename(local.path().join("src"), elsewhere.path().join("old")).unwrap();
    fs::create_dir(local.path().join("src")).unwrap();
    fs::write(local.path().join("src/app.js"), "app").unwrap();

    let changed = BTreeSet::from([PathBuf::from("src")]);
    let packed = pack_changes(0, local.path(), &rules, &changed, &mut |_| Ok(())).unwrap();

    assert_eq!(packed, None);
}

/// A rule that only ignores directories doesn't ignore a file of the same name, so a full push
/// carries the file, and its deletion must reach the sidecar, which decides by the kind that the
/// path has in the pod.
#[test]
fn a_directory_rule_doesnt_keep_a_deleted_file() {
    let local = tempfile::tempdir().unwrap();

    let packed = pack_changed(local.path(), &["cache/"], &["cache"]).unwrap();

    assert_eq!(packed.deleted, vec![b"cache".to_vec()]);
}

/// The sidecar deletes a directory with everything in it, so the paths below a deleted directory
/// need no deletion of their own.
#[test]
fn a_deleted_directory_carries_its_descendants() {
    let local = tempfile::tempdir().unwrap();

    let packed = pack_changed(local.path(), &[], &["gone", "gone/a", "gone/b/c", "kept"]).unwrap();

    assert_eq!(packed.deleted, vec![b"gone".to_vec(), b"kept".to_vec()]);
}

/// A branch switch can delete thousands of files and keep their directories, and the deleted
/// paths of an incremental push go in one `EntryDone` frame, which the operator can't decode
/// past its limit. A full push deletes by its manifest instead, which it sends in chunks.
#[test]
fn a_change_that_deletes_too_many_paths_needs_a_full_push() {
    let local = tempfile::tempdir().unwrap();
    let deleted = (0..5000)
        .map(|file| format!("packages/package/src/component_file_{file:05}.tsx"))
        .collect::<Vec<_>>();
    let deleted = deleted.iter().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(pack_changed(local.path(), &[], &deleted), None);
    assert!(pack_changed(local.path(), &[], deleted.get(..100).unwrap()).is_some());
}

/// A `.gitignore` that a developer creates above the pushed directory changes the rules too, so
/// the listing must name it even before it exists, for the CLI to watch it.
#[test]
fn a_gitignore_above_the_tree_is_a_rule_file_before_it_exists() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    fs::create_dir(repository.path().join("service")).unwrap();

    let listing = list(&repository.path().join("service"), &[]).unwrap();

    let root = repository.path().canonicalize().unwrap();
    assert!(listing.rule_files().contains(&root.join(".gitignore")));
}

/// The rules of every entry go in the push's `Begin` frame, which the operator can't decode
/// past its limit, so it would close the connection, and each push would fail the same way. The
/// push must fail with what is too large before it sends anything.
#[tokio::test]
async fn a_push_whose_rules_dont_fit_in_a_frame_fails() {
    let (mut connection, _operator) = connection().await;
    let rules = IgnoreSnapshot {
        prefix: Vec::new(),
        sources: vec![IgnoreSource {
            root: Vec::new(),
            lines: (0..30_000)
                .map(|line| format!("generated/output/file_{line:05}.js"))
                .collect(),
        }],
    };
    let entry = PackedEntry {
        plan: EntryPlan {
            index: 0,
            files: 0,
            directories: 0,
            bytes: 0,
            rules,
        },
        archive: Vec::new(),
        deleted: Vec::new(),
    };

    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        connection.push(1, PushMode::Full, vec![entry], |_| {}),
    )
    .await
    .expect("the push waited for the operator")
    .unwrap_err();

    assert!(
        matches!(error, PreviewSyncError::RulesTooLarge(_)),
        "{error}"
    );
}

/// The CLI writes a large push before it reads, and another CLI that takes the sync over makes
/// the operator send `Unpaired { Replaced }` and close the connection, which can fail one of the
/// writes first. The push must still fail as replaced, or the CLI would connect again and take
/// the sync back.
#[tokio::test]
async fn a_push_that_another_cli_replaced_fails_as_replaced() {
    let (cli, operator) = tokio::io::duplex(MAX_DATA_CHUNK_BYTES);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    let mut connection = PreviewSyncConnection::new(cli);
    let mut operator = Operator(operator);
    let large = PackedEntry {
        plan: EntryPlan {
            index: 0,
            files: 1,
            directories: 0,
            bytes: 0,
            rules: IgnoreSnapshot::default(),
        },
        archive: vec![7; 16 * MAX_DATA_CHUNK_BYTES],
        deleted: Vec::new(),
    };

    let operator = async move {
        operator.recv().await;
        operator
            .send(SyncFrame::Unpaired {
                reason: UnpairReason::Replaced,
            })
            .await;
        operator.0.close(None).await.unwrap();
    };
    let (pushed, ()) = tokio::time::timeout(
        TEST_TIMEOUT,
        futures::future::join(
            connection.push(1, PushMode::Full, vec![large], |_| {}),
            operator,
        ),
    )
    .await
    .unwrap();

    assert!(
        matches!(
            pushed,
            Err(PreviewSyncError::Unpaired(UnpairReason::Replaced))
        ),
        "{pushed:?}"
    );
}

/// The manifest of a large tree is many frames, which the CLI writes before it reads, so the
/// operator's `Unpaired { Replaced }` can arrive while the CLI still sends it, as during a push.
/// The exchange must then fail as replaced too.
#[tokio::test]
async fn a_manifest_that_another_cli_replaced_fails_as_replaced() {
    let (cli, operator) = tokio::io::duplex(MAX_DATA_CHUNK_BYTES);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    let mut connection = PreviewSyncConnection::new(cli);
    let mut operator = Operator(operator);
    let manifest = (0..30_000)
        .map(|index| ManifestPath {
            path: format!("directory-{index:05}").into_bytes(),
            kind: ManifestKind::Dir,
        })
        .collect::<Vec<_>>();

    let operator = async move {
        operator.recv().await;
        operator
            .send(SyncFrame::Unpaired {
                reason: UnpairReason::Replaced,
            })
            .await;
        operator.0.close(None).await.unwrap();
    };
    let (exchanged, ()) = tokio::time::timeout(
        TEST_TIMEOUT,
        futures::future::join(
            connection.exchange_manifest(1, vec![(0, manifest)], |_| {}),
            operator,
        ),
    )
    .await
    .unwrap();

    assert!(
        matches!(
            exchanged,
            Err(PreviewSyncError::Unpaired(UnpairReason::Replaced))
        ),
        "{exchanged:?}"
    );
}
