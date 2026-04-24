//! Session entity store.
//!
//! Holds [`Session`] base-class records (covers Agent + Accessory subclasses,
//! discriminated by the `session_type` field). Wire id field is
//! `session_uuid`.
//!
//! See [`super::EntityStore`] for the underlying apply/get logic.

use super::EntityStore;

/// Re-export of the shared [`EntityStore`] under a semantic name.
///
/// Per the design brief, each built-in entity type gets its own
/// type-named alias. Sharing the underlying impl keeps the wire dispatch
/// uniform: a new built-in type only needs a one-line file.
pub type SessionStore = EntityStore;
