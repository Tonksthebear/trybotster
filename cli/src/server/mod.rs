//! Server communication module for botster-hub.
//!
//! This module provides the API client and related types for communicating
//! with the Rails server. It handles:
//!
//! - Message polling and acknowledgment
//! - Heartbeat registration
//! - Agent notification forwarding
//! - Message parsing and dispatch to Hub actions
//!
//! # Architecture
//!
//! The [`ApiClient`] struct encapsulates all server communication, providing
//! a clean interface for the TUI application to interact with the backend.
//!
//! # Modules
//!
//! - [`client`] - HTTP client for Rails API
//! - [`types`] - Request/response data types
//! - [`messages`] - Message parsing and Hub action conversion

// Rust guideline compliant 2025-01

pub mod client;
pub mod messages;
pub mod types;

pub use client::ApiClient;
pub use messages::{message_to_hub_action, MessageContext, MessageError, ParsedMessage};
pub use types::{AgentHeartbeatInfo, MessageData, MessageResponse};
