//! Owns the pseudo-terminal, the child process attached to it, and the thread that drains it.
//!
//! The master end is the only thing that decides how big the child thinks its terminal is, so
//! [`PtyHost::resize`] is what makes the shell obey the pane rather than the window we run in.

use std::{
    io::{Read, Write},
    sync::mpsc,
    thread,
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

use super::Ev;

pub struct PtyParams {
    pub rows: u16,
    pub cols: u16,
}

pub struct PtyHost {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtyHost {
    /// Spawns `cmd` on a fresh pty sized to `rows` x `cols`, streaming everything it writes back
    /// as [`Ev::Pty`] and a final [`Ev::PtyClosed`].
    pub fn spawn(
        mut cmd: CommandBuilder,
        params: PtyParams,
        events: mpsc::Sender<Ev>,
    ) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(pty_size(params.rows, params.cols))
            .context("failed to open a pty")?;

        cmd.env("TERM", "xterm-256color");
        // These would otherwise be inherited from the window we are drawn in and win over the
        // pty's own size for anything that reads them instead of calling TIOCGWINSZ.
        cmd.env_remove("COLUMNS");
        cmd.env_remove("LINES");
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn the child")?;
        // The master reports EOF only once every slave handle is closed, so ours must not outlive
        // the spawn or we would never notice the shell exiting.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if events.send(Ev::Pty(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = events.send(Ev::PtyClosed);
        });

        let writer = pair.master.take_writer()?;

        Ok(Self {
            master: pair.master,
            writer,
            child,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// The child's process id, which is the root of the process tree the session watcher
    /// attributes mirrord sessions to. `None` if the platform does not report one.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Resizes the pty, which raises `SIGWINCH` in the child's process group.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(pty_size(rows, cols))?;
        Ok(())
    }

    pub fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self.child.wait()?)
    }
}

impl Drop for PtyHost {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
    }
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    /// Drains events until the child exits or `timeout` elapses, returning everything it printed.
    fn read_until_closed(rx: &mpsc::Receiver<Ev>, timeout: Duration) -> String {
        let mut out = String::new();
        while let Ok(ev) = rx.recv_timeout(timeout) {
            match ev {
                Ev::Pty(bytes) => out.push_str(&String::from_utf8_lossy(&bytes)),
                Ev::PtyClosed => break,
            }
        }
        out
    }

    fn sh(script: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }

    /// The point of the whole exercise: the child is sized by the pty we hand it, not by the
    /// terminal this test runs in.
    #[test]
    fn child_sees_the_size_it_was_given() {
        let (tx, rx) = mpsc::channel();
        let _host = PtyHost::spawn(sh("stty size"), PtyParams { rows: 17, cols: 61 }, tx).unwrap();

        assert!(
            read_until_closed(&rx, Duration::from_secs(10)).contains("17 61"),
            "child did not report the pty size it was spawned with",
        );
    }

    /// A resize has to reach the child as SIGWINCH, otherwise full-screen apps never reflow.
    #[test]
    fn resize_raises_sigwinch_in_the_child() {
        let (tx, rx) = mpsc::channel();
        let host = PtyHost::spawn(
            sh("trap 'stty size; exit 0' WINCH; echo ready; while true; do sleep 0.1; done"),
            PtyParams { rows: 17, cols: 61 },
            tx,
        )
        .unwrap();

        // Resizing before the trap is installed would be missed entirely.
        let mut out = String::new();
        while !out.contains("ready") {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                Ev::Pty(bytes) => out.push_str(&String::from_utf8_lossy(&bytes)),
                ev => panic!(
                    "child died before it was ready: {}",
                    matches!(ev, Ev::PtyClosed)
                ),
            }
        }

        host.resize(24, 80).unwrap();

        assert!(
            read_until_closed(&rx, Duration::from_secs(10)).contains("24 80"),
            "child was not notified of the new size",
        );
    }
}
