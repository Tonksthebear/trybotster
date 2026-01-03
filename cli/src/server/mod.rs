//! Server communication module for botster-hub.
//!
//! This module provides the API client and related types for communicating
//! with the Rails server. It handles:
//!
//! - Message polling and acknowledgment
//! - Heartbeat registration
//! - Agent notification forwarding
//!
//! # Architecture
//!
//! The [`ApiClient`] struct encapsulates all server communication, providing
//! a clean interface for the TUI application to interact with the backend.

pub mod client;
pub mod types;

pub use client::ApiClient;
pub use types::{AgentHeartbeatInfo, MessageData, MessageResponse};
