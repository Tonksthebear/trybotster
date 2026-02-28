//! Screen dimension types for terminal output.
//!
//! ANSI snapshot generation has moved to [`crate::terminal::generate_ansi_snapshot`].

/// Information about screen dimensions.
///
/// Used for debugging and reporting screen state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenInfo {
    /// Terminal height in rows.
    pub rows: u16,
    /// Terminal width in columns.
    pub cols: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_info() {
        let info = ScreenInfo { rows: 24, cols: 80 };
        assert_eq!(info.rows, 24);
        assert_eq!(info.cols, 80);
    }
}
