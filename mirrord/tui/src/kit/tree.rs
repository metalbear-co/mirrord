//! Moving a cursor through a collapsible tree.
//!
//! A screen flattens its tree into the rows it can see right now (children of collapsed groups
//! left out) and hands them to [`TreeNav`], which keeps the cursor and the collapsed groups and
//! answers the usual tree keys. The cursor is remembered by id rather than by position, so it
//! stays on the same item when a refresh adds, removes or reorders rows around it.

use std::{collections::HashSet, hash::Hash};

/// Identifies one row across refreshes.
pub trait TreeId: Clone + Eq + Hash {
    /// The group this row sits in, or `None` at the top level.
    fn parent(&self) -> Option<Self>;
}

/// Whether a row has children, and whether they are shown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fold {
    Leaf,
    Expanded,
    Collapsed,
}

/// One visible row of a flattened tree.
///
/// The rows must come in tree order, each group directly followed by its visible children.
pub trait TreeRow {
    type Id: TreeId;

    fn id(&self) -> Self::Id;

    /// How the row was drawn. Taken from the row rather than from [`TreeNav`], so the keys act on
    /// what is on screen.
    fn fold(&self) -> Fold;
}

/// The cursor and the collapsed groups of one tree.
pub struct TreeNav<Id> {
    /// The row under the cursor. Resolved against the current rows by
    /// [`TreeNav::reconcile`] before any move.
    pub selection: Option<Id>,
    collapsed: HashSet<Id>,
}

// Written out because the derive would require `Id: Default`.
impl<Id> Default for TreeNav<Id> {
    fn default() -> Self {
        Self {
            selection: None,
            collapsed: HashSet::new(),
        }
    }
}

impl<Id: TreeId> TreeNav<Id> {
    /// A tree with the cursor on `id` and nothing collapsed, the starting point tests build on.
    #[cfg(test)]
    pub fn at(id: Id) -> Self {
        Self {
            selection: Some(id),
            ..Self::default()
        }
    }

    /// Whether the group `id` is collapsed, for the screen to leave its children out when it
    /// flattens the tree.
    pub fn is_collapsed(&self, id: &Id) -> bool {
        self.collapsed.contains(id)
    }

    /// Collapses the group `id`, for trees that start folded.
    pub fn collapse(&mut self, id: Id) {
        self.collapsed.insert(id);
    }

    /// Puts the cursor back on a row that exists: the remembered row if it is still there, else
    /// its nearest surviving group (it was deleted, filtered out or collapsed away), else the
    /// first row. Returns its index, or `None` only when `rows` is empty.
    pub fn reconcile<R: TreeRow<Id = Id>>(&mut self, rows: &[R]) -> Option<usize> {
        if rows.is_empty() {
            self.selection = None;
            return None;
        }

        let mut candidate = self.selection.clone();
        while let Some(id) = candidate {
            if let Some(index) = rows.iter().position(|row| row.id() == id) {
                self.selection = Some(id);
                return Some(index);
            }
            candidate = id.parent();
        }

        self.selection = Some(rows.first()?.id());
        Some(0)
    }

    /// The row under the cursor, reconciled first. `None` only when `rows` is empty.
    pub fn focused<'a, R: TreeRow<Id = Id>>(&mut self, rows: &'a [R]) -> Option<&'a R> {
        let index = self.reconcile(rows)?;
        rows.get(index)
    }

    /// Moves the cursor by `delta` visible rows, stopping at either end.
    pub fn move_by<R: TreeRow<Id = Id>>(&mut self, rows: &[R], delta: isize) {
        let Some(current) = self.reconcile(rows) else {
            return;
        };
        let next = current
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1));
        if let Some(row) = rows.get(next) {
            self.selection = Some(row.id());
        }
    }

    /// Moves the cursor to the next (`forward`) or previous row matching `predicate`, skipping
    /// everything in between. Does nothing when there is no such row in that direction.
    pub fn jump_to<R: TreeRow<Id = Id>>(
        &mut self,
        rows: &[R],
        forward: bool,
        predicate: impl Fn(&R) -> bool,
    ) {
        let Some(current) = self.reconcile(rows) else {
            return;
        };

        let found = if forward {
            rows.iter().skip(current + 1).find(|row| predicate(row))
        } else {
            rows.iter().take(current).rev().find(|row| predicate(row))
        };

        if let Some(row) = found {
            self.selection = Some(row.id());
        }
    }

    /// Searches for a row matching `predicate`, starting just after `from` and wrapping around
    /// through the whole list. Puts the cursor on the match, or back on `from` when nothing
    /// matches, and returns whether something did.
    pub fn search_from<R: TreeRow<Id = Id>>(
        &mut self,
        rows: &[R],
        from: &Id,
        predicate: impl Fn(&R) -> bool,
    ) -> bool {
        let Some(from_index) = rows.iter().position(|row| &row.id() == from) else {
            return false;
        };

        let found = rows
            .iter()
            .cycle()
            .skip(from_index + 1)
            .take(rows.len())
            .find(|row| predicate(row));

        match found {
            Some(row) => {
                self.selection = Some(row.id());
                true
            }
            None => {
                self.selection = Some(from.clone());
                false
            }
        }
    }

    /// `Left`: collapses the group under the cursor, or moves up to the enclosing group when the
    /// cursor is on a leaf or an already collapsed group.
    pub fn collapse_or_up<R: TreeRow<Id = Id>>(&mut self, rows: &[R]) {
        let Some(row) = self.focused(rows) else {
            return;
        };
        let id = row.id();

        match row.fold() {
            Fold::Expanded => {
                self.collapsed.insert(id);
            }
            Fold::Leaf | Fold::Collapsed => {
                if let Some(parent) = id.parent() {
                    self.selection = Some(parent);
                }
            }
        }
    }

    /// `Right`: expands the collapsed group under the cursor, or moves into its first child when
    /// it is already expanded. Does nothing on a leaf.
    pub fn expand_or_down<R: TreeRow<Id = Id>>(&mut self, rows: &[R]) {
        let Some(index) = self.reconcile(rows) else {
            return;
        };
        let Some(row) = rows.get(index) else {
            return;
        };
        let id = row.id();

        match row.fold() {
            Fold::Leaf => {}
            Fold::Collapsed => {
                self.collapsed.remove(&id);
            }
            // Rows come in tree order, so an expanded group's first child is the row right after
            // it.
            Fold::Expanded => {
                if let Some(child) = rows.get(index + 1)
                    && child.id().parent().as_ref() == Some(&id)
                {
                    self.selection = Some(child.id());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fold, TreeId, TreeNav, TreeRow};

    /// A path from the root: `["a"]` is a group, `["a", "x"]` a leaf inside it.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct Path(Vec<&'static str>);

    impl TreeId for Path {
        fn parent(&self) -> Option<Self> {
            let (_, parent) = self.0.split_last()?;
            (!parent.is_empty()).then(|| Path(parent.to_vec()))
        }
    }

    struct Node(Path, Fold);

    impl TreeRow for Node {
        type Id = Path;

        fn id(&self) -> Path {
            self.0.clone()
        }

        fn fold(&self) -> Fold {
            self.1
        }
    }

    fn leaf(path: &[&'static str]) -> Node {
        Node(Path(path.to_vec()), Fold::Leaf)
    }

    /// Flattens the two-group tree the tests share, leaving out what `nav` has collapsed.
    fn rows(nav: &TreeNav<Path>) -> Vec<Node> {
        let mut rows = Vec::new();
        for (name, children) in [("a", vec!["x", "y"]), ("b", vec!["z"])] {
            let path = Path(vec![name]);
            let collapsed = nav.is_collapsed(&path);
            let fold = if collapsed {
                Fold::Collapsed
            } else {
                Fold::Expanded
            };
            rows.push(Node(path, fold));
            if !collapsed {
                rows.extend(children.into_iter().map(|child| leaf(&[name, child])));
            }
        }
        rows
    }

    fn at(path: &[&'static str]) -> Option<Path> {
        Some(Path(path.to_vec()))
    }

    #[test]
    fn moves_stop_at_the_ends() {
        let mut nav = TreeNav::default();
        nav.move_by(&rows(&nav), -3);
        assert_eq!(nav.selection, at(&["a"]));
        nav.move_by(&rows(&nav), 10);
        assert_eq!(nav.selection, at(&["b", "z"]));
    }

    #[test]
    fn left_collapses_then_climbs_and_right_reverses_it() {
        let mut nav = TreeNav::at(Path(vec!["a", "y"]));

        nav.collapse_or_up(&rows(&nav));
        assert_eq!(nav.selection, at(&["a"]));
        nav.collapse_or_up(&rows(&nav));
        assert!(nav.is_collapsed(&Path(vec!["a"])));
        assert_eq!(rows(&nav).len(), 3);

        nav.expand_or_down(&rows(&nav));
        assert!(!nav.is_collapsed(&Path(vec!["a"])));
        nav.expand_or_down(&rows(&nav));
        assert_eq!(nav.selection, at(&["a", "x"]));
    }

    #[test]
    fn a_vanished_row_falls_back_to_its_group() {
        let mut nav = TreeNav::at(Path(vec!["b", "gone"]));
        assert_eq!(nav.reconcile(&rows(&nav)), Some(3));
        assert_eq!(nav.selection, at(&["b"]));
    }

    #[test]
    fn jumps_and_wrapping_search() {
        let mut nav = TreeNav::default();
        nav.jump_to(&rows(&nav), true, |row| row.fold() != Fold::Leaf);
        assert_eq!(nav.selection, at(&["b"]));

        let from = Path(vec!["b"]);
        assert!(nav.search_from(&rows(&nav), &from, |row| row.0.0.last() == Some(&"y")));
        assert_eq!(nav.selection, at(&["a", "y"]));
        assert!(!nav.search_from(&rows(&nav), &from, |_| false));
        assert_eq!(nav.selection, Some(from));
    }
}
