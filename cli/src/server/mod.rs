//! Server communication module for botster.
//!
//! This module provides types for communicating with the Rails server:
//!
//! - Message parsing (`ParsedMessage`)
//! - Request/response data types (`MessageData`)
//! - HTTP client for Rails API (`ApiClient`)
//!
//! # Architecture
//!
//! Message delivery is handled by the WebSocket command channel, not HTTP polling.
//! The `ParsedMessage` type extracts structured data from server payloads for
//! routing in `server_comms.rs`.
//!
//! # Modules
//!
//! - [`client`] - HTTP client for Rails API
//! - [`types`] - Request/response data types
//! - [`messages`] - Message parsing

// Rust guideline compliant 2025-01

pub mod client;
pub mod messages;
pub mod types;

pub use messages::ParsedMessage;
pub use types::MessageData;
