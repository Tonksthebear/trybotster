//! Workspace entity store. Wire id field is `workspace_id`.

use super::EntityStore;

/// Backing store for `workspace` entity records. Wire id field is
/// `workspace_id`. See [`super::EntityStore`] for the apply/get semantics.
pub type WorkspaceStore = EntityStore;
