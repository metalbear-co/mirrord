//! Pure UI/cursor state for the preview environments screen: selection identity, collapse
//! sets, and filter/search mode. No I/O — everything here operates purely against
//! `view::Row` slices, so it's straightforward to unit test.

use kube::ResourceExt;

use super::view::Row;
use crate::{
    kit::tree::{TreeId, TreeNav},
    local_sessions::LocalSessions,
};

pub type TargetKey = (String, String);

/// Identity of the currently focused row, stable across background data refreshes/re-sorts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    Namespace(String),
    Target(String, TargetKey),
    Env(String, TargetKey, String),
}

impl TreeId for Selection {
    fn parent(&self) -> Option<Selection> {
        match self {
            Selection::Env(namespace, target, _) => {
                Some(Selection::Target(namespace.clone(), target.clone()))
            }
            Selection::Target(namespace, _) => Some(Selection::Namespace(namespace.clone())),
            Selection::Namespace(_) => None,
        }
    }
}

impl Selection {
    /// The literal word that must be typed to confirm stopping every environment in this
    /// scope, on top of pressing `Enter` — `None` for `Env`, where `Enter` alone is enough
    /// since a single environment is much lower-stakes than a whole target or namespace.
    pub fn stop_confirmation_word(&self) -> Option<&'static str> {
        match self {
            Selection::Env(..) => None,
            Selection::Target(..) => Some("target"),
            Selection::Namespace(..) => Some("namespace"),
        }
    }
}

/// What keyboard input currently does on this screen. `Browsing` is the default (navigate the
/// tree); the other variants are mutually exclusive modal states that each capture all input
/// until dismissed. Folded into one enum — rather than separate independent fields/booleans,
/// which is how this started with `FilterMode` plus a standalone `show_help: bool` — so that
/// mutual exclusivity is structural, not a convention `handle_event` has to maintain by hand
/// as more modal states get added.
#[derive(Clone, Debug, Default)]
pub enum Mode {
    #[default]
    Browsing,
    /// The filter text itself always lives in `UiState::filter` — keystrokes mutate it
    /// directly (so filtering is live, not just on lock-in), so this variant carries no
    /// payload of its own.
    Filtering,
    /// `g`: incremental "go to a namespace/target by name" search. `origin` is the row that
    /// was focused when `g` was pressed, restored on `Esc`. `input` is the search text typed
    /// so far. `found` is whether the last search actually matched something, driving the "no
    /// match" indicator in the go-to box.
    GoTo {
        input: String,
        origin: Selection,
        found: bool,
    },
    /// `s`: confirming which preview environment(s) to stop before actually doing it. `scope`
    /// is the selection that was focused when `s` was pressed (an `Env`, `Target`, or
    /// `Namespace`) — it drives the dialog's wording, especially making it unmistakable when
    /// the scope is "every env under this target/namespace" rather than just one. `candidates`
    /// is every session that scope actually covers, resolved once up front so the dialog shows
    /// (and, on confirm, deletes) an exact, static list even if the background watch updates
    /// the underlying data while the dialog is open. `confirmation` is the text typed so far
    /// toward `scope.stop_confirmation_word()` — always empty and irrelevant for an `Env`
    /// scope, which needs no typed confirmation.
    ConfirmStop {
        scope: Selection,
        candidates: Vec<StopCandidate>,
        confirmation: String,
    },
}

/// One preview session slated for stopping: enough to both display it in the confirmation
/// dialog and actually issue the delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopCandidate {
    pub namespace: String,
    /// The Kubernetes resource name — what the actual delete call targets.
    pub name: String,
    /// The user-facing preview key — what the confirmation dialog shows instead of the
    /// resource name, since that's what a user actually recognizes their preview by.
    pub key: String,
}

/// One env row resolved out of `envs_in_scope`, fully owned so callers don't need to know
/// about `Row`/`PreviewSession` at all.
struct ScopedEnv {
    namespace: String,
    name: String,
    key: String,
}

/// Every env that `scope` (a `Namespace`, `Target`, or `Env` selection) covers — the shared
/// scoping rule behind both `s` (stop) and `Enter` (expand/collapse), so "stop everything under
/// this target/namespace" and "expand everything under this target/namespace" always agree on
/// what "everything" means. For an `Env` scope this always resolves to exactly that one env.
fn envs_in_scope(rows: &[Row], scope: &Selection) -> Vec<ScopedEnv> {
    rows.iter()
        .filter_map(|row| {
            let Row::Env {
                namespace,
                target,
                session,
            } = row
            else {
                return None;
            };
            let matches = match scope {
                Selection::Namespace(scope_namespace) => namespace == scope_namespace,
                Selection::Target(scope_namespace, scope_target) => {
                    namespace == scope_namespace && target == scope_target
                }
                Selection::Env(scope_namespace, scope_target, scope_name) => {
                    namespace == scope_namespace
                        && target == scope_target
                        && &session.name_any() == scope_name
                }
            };
            matches.then(|| ScopedEnv {
                namespace: namespace.clone(),
                name: session.name_any(),
                key: session.spec.key.clone(),
            })
        })
        .collect()
}

#[derive(Default)]
pub struct UiState {
    /// The cursor and the collapsed namespace and target groups.
    pub tree: TreeNav<Selection>,
    pub mode: Mode,
    /// Empty string means "no filter" — `"x".contains("")` is always `true`, so no `Option`
    /// wrapper is needed anywhere this is read. Kept live-updated on every keystroke while
    /// `mode` is `Filtering`, so the visible list filters as you type.
    pub filter: String,
    pub scroll_offset: usize,
    /// "Only my sessions" (`m`): the sessions to narrow to, or `None` for every preview. The
    /// screen sets it before every draw from the interface-wide switch.
    pub mine: Option<LocalSessions>,
}

impl UiState {
    /// Puts the cursor back on a row that exists - see [`TreeNav::reconcile`]. Returns the row's
    /// index, or `None` only when `rows` is empty.
    pub fn reconcile_selection(&mut self, rows: &[Row]) -> Option<usize> {
        self.tree.reconcile(rows)
    }

    /// Moves the cursor by `delta` visible rows, clamped at both ends (no wraparound).
    pub fn move_cursor(&mut self, rows: &[Row], delta: isize) {
        self.tree.move_by(rows, delta);
    }

    /// `n`: jump to the next namespace header.
    pub fn next_namespace(&mut self, rows: &[Row]) {
        self.tree
            .jump_to(rows, true, |row| matches!(row, Row::NamespaceHeader { .. }));
    }

    /// `N`: jump to the previous namespace header.
    pub fn prev_namespace(&mut self, rows: &[Row]) {
        self.tree.jump_to(rows, false, |row| {
            matches!(row, Row::NamespaceHeader { .. })
        });
    }

    /// `t`: jump to the next target header (may cross into the next namespace).
    pub fn next_target(&mut self, rows: &[Row]) {
        self.tree
            .jump_to(rows, true, |row| matches!(row, Row::TargetHeader { .. }));
    }

    /// `T`: jump to the previous target header (may cross into the previous namespace).
    pub fn prev_target(&mut self, rows: &[Row]) {
        self.tree
            .jump_to(rows, false, |row| matches!(row, Row::TargetHeader { .. }));
    }

    /// `e`: jump to the next env card (may cross into the next target/namespace).
    pub fn next_env(&mut self, rows: &[Row]) {
        self.tree
            .jump_to(rows, true, |row| matches!(row, Row::Env { .. }));
    }

    /// `E`: jump to the previous env card (may cross into the previous target/namespace).
    pub fn prev_env(&mut self, rows: &[Row]) {
        self.tree
            .jump_to(rows, false, |row| matches!(row, Row::Env { .. }));
    }

    /// Re-runs the `GoTo` search (a no-op if `mode` isn't currently `GoTo`) against the latest
    /// `rows`, after `input`/`origin` have already been updated by the caller.
    pub fn update_goto_search(&mut self, rows: &[Row]) {
        let Mode::GoTo { input, origin, .. } = &self.mode else {
            return;
        };
        let input = input.clone();
        let origin = origin.clone();

        let matched = self.goto_search(rows, &origin, &input);

        if let Mode::GoTo { found, .. } = &mut self.mode {
            *found = matched;
        }
    }

    /// `g`: searches for a namespace/target header whose name contains `needle`, starting
    /// just after `from` and wrapping around through the whole list. Leaves the cursor at `from`
    /// when there's no match or `needle` is empty. Returns whether a match was found.
    fn goto_search(&mut self, rows: &[Row], from: &Selection, needle: &str) -> bool {
        if needle.is_empty() {
            self.tree.selection = Some(from.clone());
            return true;
        }

        // Matches on the target's *name* only, not its `kind` (e.g. "Deployment") - this is
        // "go to by name," not "go to by resource kind."
        self.tree.search_from(rows, from, |row| match row {
            Row::NamespaceHeader { namespace, .. } => namespace.contains(needle),
            Row::TargetHeader { target, .. } => target.1.contains(needle),
            Row::Env { .. } => false,
        })
    }

    /// `Left`/`h`: collapse the focused header, or move focus to the parent if the header is
    /// already collapsed (or the focus is on a leaf env card).
    pub fn collapse_or_up(&mut self, rows: &[Row]) {
        self.tree.collapse_or_up(rows);
    }

    /// `Right`/`l`: expand the focused header, or move focus into its first visible child if
    /// it's already expanded. No-op on a leaf env card.
    pub fn expand_or_down(&mut self, rows: &[Row]) {
        self.tree.expand_or_down(rows);
    }

    /// `s`: resolves every session the focused selection's scope covers (just the one env, or
    /// every env under the focused target/namespace) and, if there's at least one, enters
    /// `Mode::ConfirmStop` so the user can review the exact list before anything is actually
    /// deleted. A no-op if the tree is empty.
    pub fn request_stop(&mut self, rows: &[Row]) {
        let Some(scope) = self.tree.focused(rows).map(Row::selection) else {
            return;
        };

        let candidates: Vec<StopCandidate> = envs_in_scope(rows, &scope)
            .into_iter()
            .map(|env| StopCandidate {
                namespace: env.namespace,
                name: env.name,
                key: env.key,
            })
            .collect();

        if !candidates.is_empty() {
            self.mode = Mode::ConfirmStop {
                scope,
                candidates,
                confirmation: String::new(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    // Fixtures are built right above each assertion, so an out-of-range index is a broken test
    // rather than a reachable panic.
    #![allow(clippy::indexing_slicing)]
    use std::sync::Arc;

    use mirrord_operator::crd::{
        preview::PreviewSession,
        session::{KubeResourceTarget, SessionTarget},
    };

    use super::*;

    fn env_row(namespace: &str, kind: &str, target_name: &str, name: &str, key: &str) -> Row {
        let session = PreviewSession {
            metadata: kube::api::ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some(namespace.to_owned()),
                ..Default::default()
            },
            spec: mirrord_operator::crd::preview::PreviewSessionSpec {
                image: "test".to_owned(),
                key: key.to_owned(),
                target: SessionTarget::KubeResource(KubeResourceTarget {
                    kind: kind.to_owned(),
                    name: target_name.to_owned(),
                    ..Default::default()
                }),
                ttl_secs: 3600,
                replicas: 1,
                incoming: None,
                queue_splitting: None,
                db_branching: None,
                env: None,
                labels: None,
                config_mounts: Vec::new(),
                secret_mounts: Vec::new(),
                idle: None,
                cronjob: None,
                spec_resources: None,
                share_host: None,
            },
            status: None,
        };
        Row::Env {
            namespace: namespace.to_owned(),
            target: (kind.to_owned(), target_name.to_owned()),
            session: Arc::new(session),
        }
    }

    fn ns(name: &str, collapsed: bool, count: usize) -> Row {
        Row::NamespaceHeader {
            namespace: name.to_owned(),
            collapsed,
            env_count: count,
        }
    }

    fn target(namespace: &str, kind: &str, name: &str, collapsed: bool, count: usize) -> Row {
        Row::TargetHeader {
            namespace: namespace.to_owned(),
            target: (kind.to_owned(), name.to_owned()),
            collapsed,
            env_count: count,
        }
    }

    #[test]
    fn move_cursor_clamps_at_ends() {
        let rows = vec![ns("a", false, 1), ns("b", false, 1)];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Namespace("a".to_owned())),
            ..Default::default()
        };

        ui.move_cursor(&rows, -5);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("a".to_owned()))
        );

        ui.move_cursor(&rows, 5);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("b".to_owned()))
        );
    }

    #[test]
    fn next_and_prev_namespace_skip_over_targets_and_clamp() {
        let rows = vec![
            ns("a", false, 1),
            target("a", "Deployment", "app", false, 1),
            ns("b", false, 1),
            target("b", "Deployment", "app", false, 1),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Namespace("a".to_owned())),
            ..Default::default()
        };

        ui.next_namespace(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("b".to_owned()))
        );

        // Already on the last namespace: no further namespace to jump to, so no-op.
        ui.next_namespace(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("b".to_owned()))
        );

        ui.prev_namespace(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("a".to_owned()))
        );
    }

    #[test]
    fn next_target_crosses_into_the_next_namespace() {
        let rows = vec![
            ns("a", false, 1),
            target("a", "Deployment", "app", false, 1),
            ns("b", false, 1),
            target("b", "Deployment", "app", false, 1),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Target(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
            )),
            ..Default::default()
        };

        ui.next_target(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Target(
                "b".to_owned(),
                ("Deployment".to_owned(), "app".to_owned())
            ))
        );
    }

    #[test]
    fn next_and_prev_env_cross_into_the_next_target_and_clamp() {
        let rows = vec![
            ns("a", false, 2),
            target("a", "Deployment", "app", false, 1),
            env_row("a", "Deployment", "app", "app-pr-1", "pr-1"),
            target("a", "Deployment", "other", false, 1),
            env_row("a", "Deployment", "other", "other-pr-2", "pr-2"),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
                "app-pr-1".to_owned(),
            )),
            ..Default::default()
        };

        ui.next_env(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "other".to_owned()),
                "other-pr-2".to_owned()
            ))
        );

        // Already on the last env: no further one to jump to, so no-op.
        ui.next_env(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "other".to_owned()),
                "other-pr-2".to_owned()
            ))
        );

        ui.prev_env(&rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
                "app-pr-1".to_owned()
            ))
        );
    }

    #[test]
    fn goto_search_finds_forward_and_wraps_around() {
        let rows = vec![
            ns("prod-a", false, 1),
            ns("staging", false, 1),
            ns("prod-b", false, 1),
        ];
        let mut ui = UiState::default();
        let from = Selection::Namespace("prod-a".to_owned());

        // Nothing named "prod" comes after "prod-a" until wrapping back around to it.
        let found = ui.goto_search(&rows, &from, "prod");
        assert!(found);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("prod-b".to_owned()))
        );
    }

    #[test]
    fn goto_search_no_match_keeps_origin() {
        let rows = vec![ns("prod-a", false, 1), ns("staging", false, 1)];
        let mut ui = UiState::default();
        let from = Selection::Namespace("prod-a".to_owned());

        let found = ui.goto_search(&rows, &from, "nonexistent");
        assert!(!found);
        assert_eq!(ui.tree.selection, Some(from));
    }

    #[test]
    fn goto_search_empty_needle_resets_to_origin() {
        let rows = vec![ns("prod-a", false, 1), ns("staging", false, 1)];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Namespace("staging".to_owned())),
            ..Default::default()
        };
        let from = Selection::Namespace("prod-a".to_owned());

        let found = ui.goto_search(&rows, &from, "");
        assert!(found);
        assert_eq!(ui.tree.selection, Some(from));
    }

    #[test]
    fn goto_search_matches_target_name_not_kind() {
        let rows = vec![
            ns("a", false, 1),
            target("a", "Deployment", "checkout", false, 1),
        ];
        let mut ui = UiState::default();
        let from = Selection::Namespace("a".to_owned());

        // "Deployment" is the kind, not the name — must not match.
        assert!(!ui.goto_search(&rows, &from, "Deployment"));
        assert!(ui.goto_search(&rows, &from, "checkout"));
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Target(
                "a".to_owned(),
                ("Deployment".to_owned(), "checkout".to_owned())
            ))
        );
    }

    #[test]
    fn collapse_then_up_climbs_to_parent() {
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Target(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
            )),
            ..Default::default()
        };

        // Collapsed already (simulated via rows), so Left should climb to the namespace.
        let collapsed_rows = vec![target("a", "Deployment", "app", true, 1)];
        ui.collapse_or_up(&collapsed_rows);
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("a".to_owned()))
        );
    }

    #[test]
    fn reconcile_falls_back_to_nearest_ancestor_when_deleted() {
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
                "gone".to_owned(),
            )),
            ..Default::default()
        };

        let rows = vec![
            ns("a", false, 1),
            target("a", "Deployment", "app", false, 1),
        ];
        let index = ui.reconcile_selection(&rows);
        assert_eq!(index, Some(1));
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Target(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned())
            ))
        );
    }

    #[test]
    fn reconcile_falls_back_to_first_row_when_nothing_survives() {
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Namespace("gone".to_owned())),
            ..Default::default()
        };

        let rows = vec![ns("a", false, 1)];
        let index = ui.reconcile_selection(&rows);
        assert_eq!(index, Some(0));
        assert_eq!(
            ui.tree.selection,
            Some(Selection::Namespace("a".to_owned()))
        );
    }

    #[test]
    fn request_stop_on_an_env_scopes_to_just_that_env() {
        let rows = vec![
            ns("a", false, 2),
            target("a", "Deployment", "app", false, 2),
            env_row("a", "Deployment", "app", "app-pr-1", "pr-1"),
            env_row("a", "Deployment", "app", "app-pr-2", "pr-2"),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
                "app-pr-1".to_owned(),
            )),
            ..Default::default()
        };

        ui.request_stop(&rows);

        let Mode::ConfirmStop {
            scope, candidates, ..
        } = &ui.mode
        else {
            panic!("expected ConfirmStop mode");
        };
        assert_eq!(
            *scope,
            Selection::Env(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
                "app-pr-1".to_owned()
            )
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].key, "pr-1");
    }

    #[test]
    fn request_stop_on_a_target_scopes_to_every_env_under_it() {
        let rows = vec![
            ns("a", false, 3),
            target("a", "Deployment", "app", false, 2),
            env_row("a", "Deployment", "app", "app-pr-1", "pr-1"),
            env_row("a", "Deployment", "app", "app-pr-2", "pr-2"),
            target("a", "Deployment", "other", false, 1),
            env_row("a", "Deployment", "other", "other-pr-3", "pr-3"),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Target(
                "a".to_owned(),
                ("Deployment".to_owned(), "app".to_owned()),
            )),
            ..Default::default()
        };

        ui.request_stop(&rows);

        let Mode::ConfirmStop { candidates, .. } = &ui.mode else {
            panic!("expected ConfirmStop mode");
        };
        let mut keys: Vec<&str> = candidates.iter().map(|c| c.key.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["pr-1", "pr-2"]);
    }

    #[test]
    fn request_stop_on_a_namespace_scopes_to_every_env_in_it() {
        let rows = vec![
            ns("a", false, 2),
            target("a", "Deployment", "app", false, 1),
            env_row("a", "Deployment", "app", "app-pr-1", "pr-1"),
            target("a", "Deployment", "other", false, 1),
            env_row("a", "Deployment", "other", "other-pr-2", "pr-2"),
            ns("b", false, 1),
            target("b", "Deployment", "app", false, 1),
            env_row("b", "Deployment", "app", "app-pr-3", "pr-3"),
        ];
        let mut ui = UiState {
            tree: TreeNav::at(Selection::Namespace("a".to_owned())),
            ..Default::default()
        };

        ui.request_stop(&rows);

        let Mode::ConfirmStop { candidates, .. } = &ui.mode else {
            panic!("expected ConfirmStop mode");
        };
        let mut keys: Vec<&str> = candidates.iter().map(|c| c.key.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["pr-1", "pr-2"]);
    }
}
