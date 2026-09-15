//! Every key binding of the targets screen, in one place.
//!
//! Handlers match on these constants instead of inline characters, so the
//! full binding table is readable here and changing a key is a one-line
//! edit. If user-configurable bindings land later, this module becomes the
//! default table a config loader falls back to - the handlers already go
//! through named bindings, so nothing else has to change.
//!
//! Arrow keys, Enter, and Esc are universal TUI conventions and stay
//! hardcoded at the call sites; only the letter bindings live here.

/// Vim-style alternatives to the arrow keys, honored in every pane.
pub const UP: char = 'k';
pub const DOWN: char = 'j';
/// Vim-style alternatives to `→`/`←` for expanding/collapsing tree rows.
pub const EXPAND: char = 'l';
pub const COLLAPSE: char = 'h';

/// Start typing a filter in the target browser.
pub const FILTER: char = '/';
/// Cycle through cluster namespaces in the target browser.
pub const NAMESPACE: char = 'n';
/// Open the kube context picker (switch clusters).
pub const CONTEXT: char = 'c';
/// Refetch targets from the cluster.
pub const REFRESH: char = 'R';

/// Move focus to the right pane (plan or logs).
pub const FOCUS_PLAN: char = 'p';
/// Move focus back to the browser ("add another target").
pub const BROWSE: char = 'a';

/// Move the selected service up/down in the plan (order is preserved in
/// the emitted file).
pub const MOVE_UP: char = 'K';
pub const MOVE_DOWN: char = 'J';
/// Delete the selected service from the plan.
pub const DELETE: char = 'd';
/// Toggle `skip` on the selected service.
pub const SKIP: char = 's';
/// Open the export dialog.
pub const EXPORT: char = 'e';
/// Run the plan.
pub const RUN: char = 'r';

/// Stop the running session (SIGTERM first, SIGKILL on repeat).
pub const STOP: char = 'x';
/// Jump back to following the log tail.
pub const FOLLOW: char = 'G';

/// Toggle the key binding overlay.
pub const HELP: char = '?';
