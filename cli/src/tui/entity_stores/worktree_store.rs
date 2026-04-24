//! Worktree entity store. Wire id field is `worktree_path`.

use super::EntityStore;

/// Backing store for `worktree` entity records. Wire id field is
/// `worktree_path`. See [`super::EntityStore`] for the apply/get semantics.
pub type WorktreeStore = EntityStore;
