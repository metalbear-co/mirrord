//! Hands the console over from mirrord's progress reporting to the user's process on Windows.
//!
//! On Unix, `mirrord exec` `execve`s into the user's binary, which kills every indicatif ticker
//! thread along with the CLI. On Windows the CLI stays alive to wait for the child, and the child
//! inherits our console.
//!
//! A spinner still alive at that point keeps redrawing every tick. It moves the cursor up and
//! clears as many lines as the progress tree occupies, erasing whatever the child printed. Every
//! console mirrord runs in (conhost, and ConPTY-backed terminals such as Windows Terminal, VS Code
//! or JetBrains) shares one screen buffer between us and the child, so all of them are affected.

use mirrord_progress::{Progress, ProgressTracker};

/// The [`Progress`] handed to
/// [`LayerManagedProcess::execute`](mirrord_layer_lib::process::windows::execution::LayerManagedProcess::execute)
/// by `mirrord exec`.
///
/// Finishing it finishes the root too, task first. `execute` finishes it before the child first
/// runs, so no spinner is left redrawing over what the child prints.
pub(crate) struct ProcessHandoffProgress<'a> {
    task: ProgressTracker,
    /// Finished right after `task`, on its first success. [`None`] once finished, and in
    /// subtasks, which never own the root.
    root: Option<&'a mut ProgressTracker>,
}

impl<'a> ProcessHandoffProgress<'a> {
    /// Starts the `text` task under `root`, and takes the root over until the handoff.
    pub(crate) fn new(root: &'a mut ProgressTracker, text: &str) -> Self {
        Self {
            task: root.subtask(text),
            root: Some(root),
        }
    }
}

impl Progress for ProcessHandoffProgress<'_> {
    fn subtask(&self, text: &str) -> Self {
        Self {
            task: self.task.subtask(text),
            root: None,
        }
    }

    fn success(&mut self, msg: Option<&str>) {
        self.task.success(msg);
        if let Some(root) = self.root.take() {
            root.success(None);
        }
    }

    fn failure(&mut self, msg: Option<&str>) {
        self.task.failure(msg);
    }

    fn warning(&self, msg: &str) {
        self.task.warning(msg);
    }

    fn info(&self, msg: &str) {
        self.task.info(msg);
    }

    fn ide(&self, value: serde_json::Value) {
        self.task.ide(value);
    }

    fn print(&self, msg: &str) {
        self.task.print(msg);
    }

    fn add_to_print_buffer(&mut self, msg: &str) {
        // The root prints its buffer after the tree's last redraw. The task's would print before
        // the root finishes, and that redraw would land on it.
        match self.root.as_deref_mut() {
            Some(root) => root.add_to_print_buffer(msg),
            None => self.task.add_to_print_buffer(msg),
        }
    }

    fn set_fail_on_drop(&mut self, fail: bool) {
        self.task.set_fail_on_drop(fail);
    }

    fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        self.task.suspend(f)
    }
}
