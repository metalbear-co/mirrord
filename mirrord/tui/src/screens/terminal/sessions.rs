//! Watches the local mirrord session registry for the sessions started from the pane's shell.
//!
//! mirrord's internal proxy publishes one HTTP API per session over a unix socket in
//! `~/.mirrord/sessions`, and reports the processes running under that session. Those sockets are
//! per-user, not per-shell, so a session is attributed to this pane by walking each of its
//! processes up the process tree: the session belongs here when the pane's shell is one of the
//! ancestors. That covers the indirect cases — `mirrord exec` under `npm run`, a `make` recipe, a
//! wrapper script — and not just a command typed straight at the prompt.
//!
//! The tree is read through the operating system directly rather than by running `ps`. Forking from
//! this process is not free: it owns a terminal in raw mode and a pty, and running a child every
//! couple of seconds from the runtime that also serves the keyboard was observed to wedge the
//! application. Reading a single parent at a time is also far less work than parsing the whole
//! process table for the handful of pids a session actually has.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};

use k8s_openapi::jiff::Timestamp;
use mirrord_session_monitor_client::{
    ProcessInfo, SessionInfo, connect_to_session, session_endpoints, sessions_dir,
};
use tokio::sync::{Notify, watch};
use tracing::Level;

use crate::context::Context;

/// How often the session registry is polled.
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// How long a single session's `/info` request may take before it is treated as unreachable.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling on the walk up the process tree. The chain is rooted at pid 1 and only a handful deep,
/// so this only exists so that a cyclic parent map cannot spin the refresh task forever.
const MAX_ANCESTRY_DEPTH: usize = 64;

/// What the panel knows about the sessions running under the pane's shell.
#[derive(Debug, Default)]
pub struct Data {
    pub state: State,
    /// When [`Self::state`] was last replaced, or `None` before the first refresh.
    pub updated_at: Option<Timestamp>,
}

#[derive(Debug, Default)]
pub enum State {
    /// The shell has not been started yet, so there is nothing to attribute sessions to.
    #[default]
    NoShell,
    /// The registry was read; the sessions are the ones this shell started, oldest first.
    Ready(Vec<SessionInfo>),
    /// The registry itself could not be read. A single unreachable session is not an error —
    /// it is skipped, because a sentinel left behind by a crashed session looks exactly the same.
    ///
    /// The panel is not drawn at all without sessions to put in it, so this reaches the log rather
    /// than the screen.
    Failed(String),
}

impl Data {
    /// The sessions running under the shell, oldest first, and empty whenever the last refresh
    /// found none or could not be made at all.
    pub fn sessions(&self) -> &[SessionInfo] {
        match &self.state {
            State::Ready(sessions) => sessions,
            State::NoShell | State::Failed(_) => &[],
        }
    }
}

/// Polls the session registry in the background for as long as the application runs.
pub struct SessionWatcher {
    data: Arc<RwLock<Data>>,
    /// The pid the sessions are attributed to. `None` until the shell has been spawned.
    shell_pid: watch::Sender<Option<u32>>,
    refresh: Arc<Notify>,
}

impl SessionWatcher {
    pub fn new(context: Context) -> Self {
        let data = Arc::new(RwLock::new(Data::default()));
        let shell_pid = watch::Sender::new(None);
        let refresh = Arc::new(Notify::new());

        tokio::spawn(Self::run(
            context,
            data.clone(),
            shell_pid.subscribe(),
            refresh.clone(),
        ));

        Self {
            data,
            shell_pid,
            refresh,
        }
    }

    pub fn data(&self) -> &Arc<RwLock<Data>> {
        &self.data
    }

    /// Polls the registry now instead of waiting for the next tick.
    pub fn refresh(&self) {
        self.refresh.notify_one();
    }

    /// Points the watcher at a newly spawned shell, which also triggers an immediate refresh.
    ///
    /// Sessions attributed to the previous shell are dropped: its process tree is gone, so
    /// keeping them would leave the panel showing sessions nothing in this pane can still own.
    pub fn set_shell_pid(&self, pid: Option<u32>) {
        if *self.shell_pid.borrow() == pid {
            return;
        }

        self.data.write().unwrap().state = State::NoShell;
        self.shell_pid.send_replace(pid);
    }

    /// Refreshes [`Data`] until the application exits.
    async fn run(
        context: Context,
        data: Arc<RwLock<Data>>,
        mut shell_pid: watch::Receiver<Option<u32>>,
        refresh: Arc<Notify>,
    ) {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                changed = shell_pid.changed() => if changed.is_err() {
                    break;
                },
                _ = refresh.notified() => {},
                _ = interval.tick() => {},
            }

            let Some(pid) = *shell_pid.borrow_and_update() else {
                // A shell that has not started yet is not the same as one with no sessions, and
                // the panel says so rather than claiming the shell is idle.
                data.write().unwrap().state = State::NoShell;
                context.redraw.notify_one();
                continue;
            };

            let state = match sessions_dir() {
                Some(sessions_dir) => Self::fetch(pid, &sessions_dir).await,
                None => State::Failed("could not determine the home directory".to_owned()),
            };

            // Nothing draws this, so the log is the only place it can be seen.
            if let State::Failed(error) = &state {
                tracing::warn!(%error, "Could not read the local mirrord session registry.");
            }

            {
                let mut data = data.write().unwrap();
                data.state = state;
                data.updated_at = Some(Timestamp::now());
            }

            context.redraw.notify_one();
        }
    }

    /// Reads every session in `sessions_dir` and keeps the ones running under `shell_pid`.
    #[tracing::instrument(level = Level::DEBUG, ret)]
    async fn fetch(shell_pid: u32, sessions_dir: &Path) -> State {
        let endpoints = session_endpoints(sessions_dir);
        if endpoints.is_empty() {
            return State::Ready(Vec::new());
        }

        // What each session recorded about its own processes, which is all that is left of the
        // ones that have already exited.
        let mut recorded = HashMap::new();
        let mut sessions = Vec::new();

        for (session_id, endpoint) in endpoints {
            let connection =
                tokio::time::timeout(REQUEST_TIMEOUT, connect_to_session(&endpoint.sentinel_path))
                    .await;

            let info = match connection {
                Ok(Ok(connection)) => connection.info,
                // A sentinel whose session has ended looks exactly like one that is merely slow to
                // answer, and removing it is `mirrord session`'s job, not a viewer's.
                Ok(Err(error)) => {
                    tracing::debug!(%session_id, %error, "Failed to read a local mirrord session.");
                    continue;
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        %session_id,
                        "Local mirrord session did not answer within {REQUEST_TIMEOUT:?}.",
                    );
                    continue;
                }
            };

            record_parents(&info, &mut recorded);

            if descends_from(&info, shell_pid, &recorded) {
                sessions.push(info);
            }
        }

        // Refreshes must not reshuffle the panel.
        sessions.sort_by(|left, right| {
            (&left.started_at, &left.session_id).cmp(&(&right.started_at, &right.session_id))
        });

        State::Ready(sessions)
    }
}

/// The process a session is best described by: the topmost one it knows about.
pub fn primary_process(session: &SessionInfo) -> Option<&ProcessInfo> {
    let known: HashSet<_> = session
        .processes
        .iter()
        .map(|process| process.pid)
        .collect();

    session
        .processes
        .iter()
        .find(|process| {
            process
                .parent_pid
                .is_none_or(|parent_pid| !known.contains(&parent_pid))
        })
        .or_else(|| session.processes.iter().min_by_key(|process| process.pid))
}

/// Adds the parents the session recorded for its own processes to `recorded`.
///
/// The registry remembers processes that have already exited, and the operating system no longer
/// does. Keeping what the session recorded means such a process is still linked to its parent, so a
/// session does not drop off the panel just because the command it ran has finished.
fn record_parents(session: &SessionInfo, recorded: &mut HashMap<u32, u32>) {
    for process in &session.processes {
        if let Some(parent_pid) = process.parent_pid {
            recorded.entry(process.pid).or_insert(parent_pid);
        }
    }
}

/// Whether any of the session's processes runs under `ancestor`.
fn descends_from(session: &SessionInfo, ancestor: u32, recorded: &HashMap<u32, u32>) -> bool {
    // The live tree comes first: a recycled pid would otherwise be linked to a parent long gone.
    let parent_of = |pid| parent_pid(pid).or_else(|| recorded.get(&pid).copied());

    session
        .processes
        .iter()
        .any(|process| is_descendant(process.pid, ancestor, parent_of))
}

/// Walks `pid` up the process tree looking for `ancestor`.
fn is_descendant(mut pid: u32, ancestor: u32, parent_of: impl Fn(u32) -> Option<u32>) -> bool {
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if pid == ancestor {
            return true;
        }

        match parent_of(pid) {
            // pid 0 is the root of the tree, and a process that is its own parent would loop.
            Some(parent) if parent != 0 && parent != pid => pid = parent,
            _ => return false,
        }
    }

    false
}

/// A single process's parent, straight from the kernel.
#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    // `/proc/<pid>/stat` is `pid (comm) state ppid ...`, and `comm` is an unescaped executable name
    // that may itself contain spaces and brackets — so the fields are counted from the last `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// A single process's parent, straight from the kernel.
#[cfg(target_vendor = "apple")]
fn parent_pid(pid: u32) -> Option<u32> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();

    // SAFETY: `proc_pidinfo` writes at most `size` bytes through the pointer and returns how many
    // it wrote. The struct is only read when it says it filled the whole thing.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };

    // A pid that has exited (or belongs to another user) reports nothing rather than failing.
    (written == size as libc::c_int).then(|| unsafe { info.assume_init() }.pbi_ppid)
}

/// Every other platform falls back to what the sessions recorded about themselves.
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent_pid: Option<u32>) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid,
            process_name: format!("process-{pid}"),
            cmdline: Vec::new(),
        }
    }

    fn session(processes: Vec<ProcessInfo>) -> SessionInfo {
        SessionInfo {
            session_id: "abc".to_owned(),
            key: None,
            target: "deployment/api".to_owned(),
            namespace: None,
            context: None,
            started_at: "2020-01-01T00:00:00Z".to_owned(),
            mirrord_version: "3.249.0".to_owned(),
            is_operator: true,
            processes,
            port_subscriptions: Vec::new(),
            config: serde_json::Value::Null,
        }
    }

    /// The tree as the operating system would report it.
    fn tree(links: [(u32, u32); 4]) -> impl Fn(u32) -> Option<u32> {
        let links = HashMap::from(links);
        move |pid| links.get(&pid).copied()
    }

    /// The parent lookup has to agree with the kernel about a process this test actually owns.
    #[test]
    fn a_real_child_reports_this_process_as_its_parent() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let parent = parent_pid(child.id());

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            parent,
            Some(std::process::id()),
            "the kernel should report this test process as the child's parent",
        );
    }

    #[test]
    fn a_process_that_does_not_exist_has_no_parent() {
        // pid 0 is never a process this can read, so the lookup must say so rather than guess.
        assert_eq!(parent_pid(u32::MAX), None);
    }

    #[test]
    fn a_process_started_indirectly_is_still_attributed_to_the_shell() {
        // shell(501) -> npm(600) -> mirrord(700) -> node(800), which is the case a check against
        // the immediate parent alone would miss.
        let tree = tree([(600, 501), (700, 600), (800, 700), (501, 1)]);

        assert!(is_descendant(800, 501, tree));
    }

    #[test]
    fn the_shell_itself_counts_as_its_own_ancestor() {
        assert!(is_descendant(501, 501, |_| None));
    }

    #[test]
    fn another_shells_session_is_not_attributed_here() {
        // Both shells descend from the same terminal, so the walk has to stop at the shell and
        // not keep climbing into the shared ancestor.
        let tree = tree([(800, 700), (700, 502), (502, 400), (501, 400)]);

        assert!(!is_descendant(800, 501, tree));
    }

    #[test]
    fn a_session_with_no_live_processes_falls_back_to_the_recorded_parents() {
        // Neither pid exists any more, so the kernel has nothing; only what the session recorded
        // still links 800 to 700 to the shell.
        let session = session(vec![process(800, Some(700))]);
        let mut recorded = HashMap::from([(700, 501)]);

        record_parents(&session, &mut recorded);

        assert_eq!(recorded, HashMap::from([(700, 501), (800, 700)]));
        assert!(is_descendant(800, 501, |pid| recorded.get(&pid).copied()));
    }

    #[test]
    fn a_recorded_parent_never_overwrites_one_already_known() {
        // Two sessions reporting the same pid must not fight over it, and the first reading wins.
        let mut recorded = HashMap::from([(800, 501)]);

        record_parents(&session(vec![process(800, Some(999))]), &mut recorded);

        assert_eq!(recorded.get(&800), Some(&501));
    }

    #[test]
    fn a_cyclic_parent_map_terminates() {
        let tree = tree([(800, 700), (700, 800), (1, 0), (2, 0)]);

        assert!(!is_descendant(800, 501, tree));
    }

    #[test]
    fn the_primary_process_is_the_topmost_one_the_session_knows() {
        let session = session(vec![
            process(800, Some(700)),
            process(700, Some(600)),
            process(900, Some(800)),
        ]);

        assert_eq!(
            primary_process(&session).map(|process| process.pid),
            Some(700),
            "the process whose parent is outside the session should be the primary one",
        );
    }
    /// Serves one `SessionInfo` over a unix socket the way intproxy's session monitor does, so the
    /// discovery path is exercised through the real client rather than around it.
    #[cfg(unix)]
    async fn fake_session(sessions_dir: &Path, session_id: &str, info: SessionInfo) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let socket = sessions_dir.join(format!("{session_id}.sock"));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        tokio::spawn(async move {
            let body = serde_json::to_vec(&info).unwrap();

            while let Ok((mut stream, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    // The client sends a bodyless GET, which arrives in one read.
                    let mut request = [0u8; 1024];
                    let _ = stream.read(&mut request).await;

                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                        body.len(),
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.flush().await;
                });
            }
        });
    }

    /// The whole path end to end: discover the sentinels, read each session over its socket, and
    /// keep only the one whose process tree runs under this process.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_the_sessions_under_this_process_are_kept() {
        let sessions_dir = std::env::temp_dir().join(format!("mirrord-tui-{}", std::process::id()));
        // A leftover directory from a previous run would serve its stale sockets into this one.
        let _ = std::fs::remove_dir_all(&sessions_dir);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // A real child, so the attribution walks the kernel's tree rather than matching the pid
        // it was handed outright.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let mut ours = session(vec![process(child.id(), None)]);
        ours.target = "deployment/ours".to_owned();
        let mut theirs = session(vec![process(1, None)]);
        theirs.target = "deployment/theirs".to_owned();

        fake_session(&sessions_dir, "ours", ours).await;
        fake_session(&sessions_dir, "theirs", theirs).await;

        let state = SessionWatcher::fetch(std::process::id(), &sessions_dir).await;

        let _ = std::fs::remove_dir_all(&sessions_dir);
        let _ = child.kill();
        let _ = child.wait();

        let State::Ready(sessions) = state else {
            panic!("the registry should have been readable, got {state:?}");
        };
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.target.as_str())
                .collect::<Vec<_>>(),
            ["deployment/ours"],
            "only the session running under this process should be listed",
        );
    }

    /// A sentinel whose session has gone away is skipped, not reported as a failure — the panel
    /// would otherwise show an error for every crashed session the user has ever had.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_sentinel_is_skipped_rather_than_failing_the_refresh() {
        let sessions_dir =
            std::env::temp_dir().join(format!("mirrord-tui-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sessions_dir);
        std::fs::create_dir_all(&sessions_dir).unwrap();
        // A socket file nothing is listening on is exactly what a crashed session leaves behind.
        std::fs::write(sessions_dir.join("crashed.sock"), b"").unwrap();

        let state = SessionWatcher::fetch(std::process::id(), &sessions_dir).await;

        let _ = std::fs::remove_dir_all(&sessions_dir);

        assert!(
            matches!(&state, State::Ready(sessions) if sessions.is_empty()),
            "a stale sentinel should leave an empty list, got {state:?}",
        );
    }
}
