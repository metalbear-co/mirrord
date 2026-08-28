//! Runs `mirrord up` as a child process and streams its output into a pane.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
};

use crate::{
    context::Context,
    screens::targets::{keys, model::UpFile, theme},
};

enum LaunchStatus {
    Running { pid: u32 },
    Exited { message: String, success: bool },
}

/// Signs in the output that a previous mirrord session sabotaged this run.
///
/// A lingering session holds the steal lock on the target; the agent then
/// refuses this run's subscription and mirrord kills the injected process
/// with SIGTERM. The explicit lock error names the session; the SIGTERM
/// crash alone is the fallback sign when it doesn't reach our output.
#[derive(Default)]
struct ConflictSigns {
    sessions: Vec<String>,
    sigterm_crash: bool,
}

/// A lingering-session conflict, ready for the "kill and relaunch?"
/// prompt. Empty `sessions` means the ids are unknown and cleanup falls
/// back to `session kill-all`.
pub struct Conflict {
    pub sessions: Vec<String>,
    /// Found by the pre-run check (vs blamed for a crashed run).
    pub pre_launch: bool,
}

/// One `mirrord up` run: the child's output and lifecycle.
pub struct Launch {
    logs: Arc<RwLock<Vec<String>>>,
    status: Arc<RwLock<LaunchStatus>>,
    conflicts: Arc<RwLock<ConflictSigns>>,
    config_path: PathBuf,
    /// Manual scroll offset; `None` follows the tail.
    scroll: Option<usize>,
    term_sent: bool,
    /// The conflict prompt fires at most once per run.
    conflict_prompted: bool,
}

/// Extracts the session id from mirrord's steal-lock conflict error, e.g.
/// `... conflicts with existing steal(...) lock in session 7B2B150D46E62EF`.
fn conflict_session_id(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("lock in session ")?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Records conflict signs found in one output line.
fn scan_conflict(line: &str, signs: &RwLock<ConflictSigns>) {
    if let Some(id) = conflict_session_id(line) {
        if let Ok(mut signs) = signs.write()
            && !signs.sessions.contains(&id)
        {
            signs.sessions.push(id);
        }
    } else if line.contains("crashed")
        && line.contains("SIGTERM")
        && let Ok(mut signs) = signs.write()
    {
        signs.sigterm_crash = true;
    }
}

/// Operator sessions whose target matches one of the plan's targets,
/// parsed from the `mirrord operator status` table. Any failure yields an
/// empty list: this pre-run check must never stop a launch on its own.
pub fn lingering_sessions(targets: &[(String, Option<String>)]) -> Vec<String> {
    let Ok(binary) = mirrord_binary() else {
        return Vec::new();
    };
    let Ok(output) = std::process::Command::new(&binary)
        .args(["operator", "status"])
        .stdin(Stdio::null())
        .output()
    else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(&output.stdout);
    session_rows(&text)
        .filter(|(_, target, namespace)| {
            targets.iter().any(|(path, plan_namespace)| {
                kind_and_name(path) == kind_and_name(target)
                    && plan_namespace
                        .as_deref()
                        .is_none_or(|expected| expected == *namespace)
            })
        })
        .map(|(id, ..)| id.to_owned())
        .collect()
}

/// Parses `(id, target, namespace)` out of the status table's session
/// rows, skipping headers and dividers.
fn session_rows(table: &str) -> impl Iterator<Item = (&str, &str, &str)> {
    table.lines().filter_map(|line| {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let [_, id, target, namespace, ..] = cells.as_slice() else {
            return None;
        };
        let header = *id == "Session ID";
        (!header && !id.is_empty() && !target.is_empty()).then_some((*id, *target, *namespace))
    })
}

/// The `kind/name` prefix of a target path, dropping any container part,
/// so `deployment/web/container/app` matches a session on `deployment/web`.
fn kind_and_name(path: &str) -> String {
    path.split('/').take(2).collect::<Vec<_>>().join("/")
}

/// Kills lingering operator sessions: the given ids, or every session of
/// this user when none are known. Blocks until done - each kill is one
/// short operator API call.
pub fn kill_sessions(sessions: &[String]) -> anyhow::Result<()> {
    let binary = mirrord_binary()?;
    let commands: Vec<Vec<String>> = if sessions.is_empty() {
        vec![vec![
            "operator".to_owned(),
            "session".to_owned(),
            "kill-all".to_owned(),
        ]]
    } else {
        sessions
            .iter()
            .map(|id| {
                vec![
                    "operator".to_owned(),
                    "session".to_owned(),
                    "kill".to_owned(),
                    "--id".to_owned(),
                    id.clone(),
                ]
            })
            .collect()
    };

    for args in commands {
        let status = std::process::Command::new(&binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            anyhow::bail!("`mirrord {}` failed with {status}", args.join(" "));
        }
    }
    Ok(())
}

/// True when `binary` resolves the way spawning it from `dir` would: a
/// path with a separator must exist as a file (relative ones against
/// `dir`), a bare name must be in a PATH directory.
pub fn command_found(binary: &str, dir: &Path) -> bool {
    if binary.contains('/') {
        let path = Path::new(binary);
        return if path.is_absolute() {
            path.is_file()
        } else {
            dir.join(path).is_file()
        };
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

/// Finds the mirrord CLI binary, first hit wins:
/// - `MIRRORD_BIN` (the local-sandbox convention; also the escape hatch for people whose `mirrord`
///   is a shell alias - a spawned child process cannot see aliases, only real files on PATH)
/// - `mirrord` in a PATH directory
/// - common install locations that GUI-launched processes often miss
fn mirrord_binary() -> anyhow::Result<PathBuf> {
    if let Ok(binary) = std::env::var("MIRRORD_BIN") {
        return Ok(PathBuf::from(binary));
    }

    let path_dirs = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    let well_known = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    path_dirs
        .into_iter()
        .chain(well_known)
        .map(|dir| dir.join("mirrord"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "mirrord CLI not found on PATH - if `mirrord` works in your shell it may \
                 be an alias, which other programs can't see; set MIRRORD_BIN to the real \
                 binary path"
            )
        })
}

impl Launch {
    /// Writes the plan to a temp file and spawns `mirrord up -f` on it.
    /// `note` opens the log pane (e.g. what cleanup preceded this run).
    pub fn start(context: Context, file: &UpFile, note: Option<String>) -> anyhow::Result<Self> {
        let config_path =
            std::env::temp_dir().join(format!("mirrord-up-tui-{}.yaml", std::process::id()));
        std::fs::write(&config_path, file.to_yaml()?)?;

        let binary = mirrord_binary()?;

        // Simple progress prints plain lines instead of spinner control
        // sequences, which would garble the log pane.
        let mut command = Command::new(&binary);
        command
            .arg("up")
            .arg("-f")
            .arg(&config_path)
            .env("MIRRORD_PROGRESS_MODE", "simple")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // The mirrord CLI injects MIRRORD_LAYER_FILE into every launched
        // service instead of its embedded layer - a development override
        // that dev workspaces set globally (e.g. through cargo's [env]).
        // Passing through a path that does not exist makes dyld abort
        // every service, so drop it and let the CLI use its embedded layer.
        let mut layer_note = None;
        if let Ok(layer) = std::env::var("MIRRORD_LAYER_FILE")
            && !Path::new(&layer).is_file()
        {
            command.env_remove("MIRRORD_LAYER_FILE");
            layer_note = Some(format!(
                "note: MIRRORD_LAYER_FILE points to missing file {layer} - ignoring it, \
                 mirrord will use its embedded layer"
            ));
        }

        let mut child = command.spawn().map_err(|error| {
            anyhow::anyhow!("failed to start `{} up`: {error}", binary.display())
        })?;

        let pid = child.id().unwrap_or_default();
        tracing::debug!(binary = %binary.display(), pid, config = %config_path.display(), "spawned mirrord up");
        let mut first_lines = vec![format!("$ mirrord up -f {}", config_path.display())];
        first_lines.extend(note);
        first_lines.extend(layer_note);
        let logs = Arc::new(RwLock::new(first_lines));
        let status = Arc::new(RwLock::new(LaunchStatus::Running { pid }));

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let conflicts = Arc::new(RwLock::new(ConflictSigns::default()));
        {
            let logs = logs.clone();
            let status = status.clone();
            let conflicts = conflicts.clone();
            tokio::spawn(async move {
                let out = pump(stdout, logs.clone(), conflicts.clone(), context.clone());
                let err = pump(stderr, logs.clone(), conflicts.clone(), context.clone());
                let (_, _, waited) = tokio::join!(out, err, child.wait());
                tracing::debug!(?waited, "mirrord up finished");

                let (message, success) = match waited {
                    Ok(exit) if exit.success() => ("exited cleanly".to_owned(), true),
                    Ok(exit) => (format!("exited with {exit}"), false),
                    Err(error) => (format!("failed to wait: {error}"), false),
                };
                if let Ok(mut status) = status.write() {
                    *status = LaunchStatus::Exited { message, success };
                }
                context.request_redraw();
            });
        }

        Ok(Self {
            logs,
            status,
            conflicts,
            config_path,
            scroll: None,
            term_sent: false,
            conflict_prompted: false,
        })
    }

    pub fn running(&self) -> bool {
        matches!(
            self.status.read().as_deref(),
            Ok(LaunchStatus::Running { .. })
        )
    }

    /// After a failed run that looks like a lingering-session conflict,
    /// returns it exactly once so the screen can prompt for cleanup.
    /// Stops the user requested don't count - that SIGTERM was theirs.
    pub fn conflict(&mut self) -> Option<Conflict> {
        if self.running() || self.term_sent || self.conflict_prompted {
            return None;
        }
        let signs = self.conflicts.read().ok()?;
        if signs.sessions.is_empty() && !signs.sigterm_crash {
            return None;
        }
        self.conflict_prompted = true;
        Some(Conflict {
            sessions: signs.sessions.clone(),
            pre_launch: false,
        })
    }

    /// First stop asks nicely (SIGTERM lets `mirrord up` tear down its child
    /// sessions); a second stop force-kills.
    pub fn stop(&mut self) {
        let pid = match self.status.read().as_deref() {
            Ok(LaunchStatus::Running { pid }) => *pid,
            _ => return,
        };

        let signal = if self.term_sent { "-KILL" } else { "-TERM" };
        self.term_sent = true;
        _ = std::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .status();
    }

    /// Handles a key; returns `true` when the pane is done and should close.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let lines = self.logs.read().map(|logs| logs.len()).unwrap_or(0);
        match key.code {
            KeyCode::Char(keys::STOP) => self.stop(),
            KeyCode::Up | KeyCode::Char(keys::UP) => {
                let current = self.scroll.unwrap_or(lines);
                self.scroll = Some(current.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char(keys::DOWN) => {
                self.scroll = match self.scroll {
                    Some(offset) if offset + 1 < lines => Some(offset + 1),
                    _ => None,
                };
            }
            KeyCode::End | KeyCode::Char(keys::FOLLOW) => self.scroll = None,
            KeyCode::Esc if !self.running() => {
                _ = std::fs::remove_file(&self.config_path);
                return true;
            }
            _ => {}
        }
        false
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let border = if focused {
            theme::BRAND
        } else {
            theme::BORDER_DIM
        };

        let (title, title_color) = match self.status.read().as_deref() {
            Ok(LaunchStatus::Running { pid }) => (
                format!(" ● mirrord up · running (pid {pid}) "),
                theme::BRAND,
            ),
            Ok(LaunchStatus::Exited {
                message,
                success: true,
            }) => (format!(" ✓ mirrord up · {message} "), theme::SUCCESS),
            Ok(LaunchStatus::Exited { message, .. }) => {
                (format!(" ✗ mirrord up · {message} "), theme::WARNING)
            }
            Err(_) => (" mirrord up ".to_owned(), theme::TEXT_MUTED),
        };

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(Span::styled(title, Style::default().fg(title_color).bold()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Ok(logs) = self.logs.read() else { return };
        let width = (inner.width as usize).max(1);
        let height = inner.height as usize;
        let bottom = match self.scroll {
            Some(offset) => offset.min(logs.len()),
            None => logs.len(),
        };

        // Long lines wrap instead of clipping at the pane edge. Rows are
        // collected newest-first from the anchor line so the pane always
        // ends exactly at `bottom`, then flipped for display.
        let mut rows: Vec<Line> = Vec::new();
        for line in logs.iter().take(bottom).rev() {
            for chunk in wrap_chunks(line, width).into_iter().rev() {
                rows.push(Line::styled(
                    chunk,
                    Style::default().fg(theme::TEXT_EMPHASIS),
                ));
            }
            if rows.len() >= height {
                break;
            }
        }
        rows.truncate(height);
        rows.reverse();
        frame.render_widget(Paragraph::new(rows), inner);
    }
}

/// Splits a log line into display rows of at most `width` characters. An
/// empty line still yields one row so blank lines keep their spacing.
fn wrap_chunks(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    line.chars()
        .collect::<Vec<_>>()
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Streams one of the child's output pipes into the shared log buffer.
async fn pump(
    reader: Option<impl AsyncRead + Unpin>,
    logs: Arc<RwLock<Vec<String>>>,
    conflicts: Arc<RwLock<ConflictSigns>>,
    context: Context,
) {
    let Some(reader) = reader else { return };
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(line, "mirrord up output");
        scan_conflict(&line, &conflicts);
        if let Ok(mut logs) = logs.write() {
            logs.push(line);
        }
        context.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status table parser picks session rows and skips furniture.
    #[test]
    fn session_rows_reads_the_status_table() {
        let table = "\
Operator Nonhuman (CI/Preview) Concurrent Sessions:
+------------------+-------------+-----------+------+-------+----------+
| Session ID       | Target      | Namespace | User | Ports | Duration |
+------------------+-------------+-----------+------+-------+----------+
| 424FFCAA8910B92  | pod/zoo-pod | tui-zoo   | me   |       | 1h       |
+------------------+-------------+-----------+------+-------+----------+";
        assert_eq!(
            session_rows(table).collect::<Vec<_>>(),
            [("424FFCAA8910B92", "pod/zoo-pod", "tui-zoo")],
        );
    }

    #[test]
    fn kind_and_name_drops_the_container_part() {
        assert_eq!(
            kind_and_name("deployment/web/container/app"),
            "deployment/web"
        );
        assert_eq!(kind_and_name("pod/zoo-pod"), "pod/zoo-pod");
    }

    /// The parser reads the id straight out of the operator's lock error.
    #[test]
    fn conflict_session_id_reads_the_lock_error() {
        let line = "agent closed connection with error: cannot create steal(header=x) lock \
                    on port 5678 (pod abc): conflicts with existing steal(header=x) lock in \
                    session 7B2B150D46E62EF";
        assert_eq!(
            conflict_session_id(line).as_deref(),
            Some("7B2B150D46E62EF")
        );
        assert_eq!(conflict_session_id("zoo-web: Ready!"), None);
    }
}
