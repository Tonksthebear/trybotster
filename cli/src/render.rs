//! Rendering utilities for the TUI (legacy)
//!
//! Note: This module previously contained `render_agent_terminal()` which rendered
//! using the agent's internal vt100 parser. That function has been removed as part
//! of the Phase 5 migration.
//!
//! The new architecture has each client (TuiRunner, TuiClient) own their own parser.
//! TuiRunner uses `tui/render.rs` with `RenderContext.active_parser`.

// Rust guideline compliant 2026-01
