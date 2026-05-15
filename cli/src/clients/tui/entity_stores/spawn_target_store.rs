//! Spawn-target entity store. Wire id field is `target_id`.

use super::EntityStore;

/// Backing store for `spawn_target` entity records. Wire id field is
/// `target_id`. See [`super::EntityStore`] for the apply/get semantics.
pub type SpawnTargetStore = EntityStore;
