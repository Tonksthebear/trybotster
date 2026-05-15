//! Client renderer and transport adapters.
//!
//! These modules are clients of the Botster runtime. They are kept outside the
//! CLI command namespace because the `botster` binary includes them for
//! convenience, but they are not command implementation details.

pub mod tui;
