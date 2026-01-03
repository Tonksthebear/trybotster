//! Agent lifecycle management for botster-hub.
//!
//! This module provides types and utilities for managing agent lifecycles,
//! including spawning configuration and session key generation.
//!
//! # Overview
//!
//! Agents are the core units of work in botster-hub. Each agent runs in its
//! own PTY and handles a specific task (usually tied to a GitHub issue or PR).
//!
//! # Session Keys
//!
//! Each agent has a unique session key that identifies it. The key format is:
//! `{repo-safe}-{issue_number}` or `{repo-safe}-{branch_name}`
//!
//! For example: `owner-repo-42` or `owner-repo-feature-branch`

pub mod spawn;

pub use spawn::{AgentSpawnConfig, SessionKeyGenerator};
