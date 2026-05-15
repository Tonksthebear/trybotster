//! Hub-singleton entity stores. Both `hub` (recovery state) and
//! `connection_code` are modelled as singleton entity types whose id is
//! the hub_id, per design brief §3 + §12.7. Each store typically holds
//! exactly one entity.

use super::EntityStore;

/// Backing store for the singleton `hub` entity type (lifecycle / recovery
/// state). Id field is `hub_id`.
pub type HubMetaStore = EntityStore;

/// Backing store for the singleton `connection_code` entity type (pairing
/// QR + URL). Id field is `hub_id`.
pub type ConnectionCodeStore = EntityStore;
