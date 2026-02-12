//! Runtime environment detection.
//!
//! Provides a single source of truth for determining the runtime environment
//! (test, development, production) based on the `BOTSTER_ENV` environment variable.
//!
//! # Usage
//!
//! ```rust
//! use botster::env::{Environment, is_test_mode};
//!
//! // Check current environment
//! if Environment::current().is_test() {
//!     // Skip keyring, auth, etc.
//! }
//!
//! // Or use the convenience function
//! if is_test_mode() {
//!     // Test-specific behavior
//! }
//! ```
//!
//! # Environment Variable
//!
//! Set `BOTSTER_ENV` to one of:
//! - `test` - Test mode (skips auth, uses file storage instead of keyring)
//! - `system_test` - System test mode (full auth with test server, uses file storage)
//! - `development` or `dev` - Development mode
//! - (anything else or unset) - Production mode

/// Runtime environment for the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Production environment (default).
    Production,
    /// Development environment.
    Development,
    /// Test environment - skips auth, uses file storage.
    Test,
    /// System test environment - full auth with test server, uses file storage.
    /// Used for Rails system tests that spawn the CLI.
    SystemTest,
}

impl Environment {
    /// Detect current environment from `BOTSTER_ENV`.
    ///
    /// Returns `Test` if `BOTSTER_ENV=test`, `SystemTest` if `BOTSTER_ENV=system_test`,
    /// `Development` if `BOTSTER_ENV=development` or `BOTSTER_ENV=dev`, otherwise `Production`.
    #[must_use]
    pub fn current() -> Self {
        match std::env::var("BOTSTER_ENV").as_deref() {
            Ok("test") => Self::Test,
            Ok("system_test") => Self::SystemTest,
            Ok("development") | Ok("dev") => Self::Development,
            _ => Self::Production,
        }
    }

    /// Returns `true` if this is the test environment (unit tests).
    #[must_use]
    pub fn is_test(self) -> bool {
        self == Self::Test
    }

    /// Returns `true` if this is the system test environment (Rails system tests).
    #[must_use]
    pub fn is_system_test(self) -> bool {
        self == Self::SystemTest
    }

    /// Returns `true` if running in any test mode (test or system_test).
    /// Use this to skip OS keyring and use file storage instead.
    #[must_use]
    pub fn is_any_test(self) -> bool {
        matches!(self, Self::Test | Self::SystemTest)
    }

    /// Returns `true` if this is the production environment.
    #[must_use]
    pub fn is_production(self) -> bool {
        self == Self::Production
    }

    /// Returns `true` if this is the development environment.
    #[must_use]
    pub fn is_development(self) -> bool {
        self == Self::Development
    }
}

impl std::fmt::Display for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Production => write!(f, "production"),
            Self::Development => write!(f, "development"),
            Self::Test => write!(f, "test"),
            Self::SystemTest => write!(f, "system_test"),
        }
    }
}

/// Convenience function to check if running in test mode (unit tests only).
///
/// Equivalent to `Environment::current().is_test()`.
/// For keyring bypass, use `should_skip_keyring()` instead.
#[must_use]
pub fn is_test_mode() -> bool {
    Environment::current().is_test()
}

/// Returns `true` if running in any test mode (unit tests or system tests).
///
/// Use this for timeouts, intervals, file path fallbacks, etc.
/// Returns true for both `BOTSTER_ENV=test` and `BOTSTER_ENV=system_test`.
#[must_use]
pub fn is_any_test() -> bool {
    Environment::current().is_any_test()
}

/// Returns `true` if keyring should be bypassed (any test mode).
///
/// Use this instead of `is_test_mode()` when deciding whether to use
/// OS keyring vs file storage. Returns true for both `BOTSTER_ENV=test`
/// and `BOTSTER_ENV=system_test`.
#[must_use]
pub fn should_skip_keyring() -> bool {
    is_any_test()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_display() {
        assert_eq!(Environment::Production.to_string(), "production");
        assert_eq!(Environment::Development.to_string(), "development");
        assert_eq!(Environment::Test.to_string(), "test");
        assert_eq!(Environment::SystemTest.to_string(), "system_test");
    }

    #[test]
    fn test_environment_is_methods() {
        assert!(Environment::Test.is_test());
        assert!(!Environment::Test.is_production());
        assert!(!Environment::Test.is_development());
        assert!(!Environment::Test.is_system_test());

        assert!(Environment::Production.is_production());
        assert!(!Environment::Production.is_test());

        assert!(Environment::Development.is_development());
        assert!(!Environment::Development.is_test());

        assert!(Environment::SystemTest.is_system_test());
        assert!(!Environment::SystemTest.is_test());
        assert!(!Environment::SystemTest.is_production());
    }

    #[test]
    fn test_is_any_test() {
        assert!(Environment::Test.is_any_test());
        assert!(Environment::SystemTest.is_any_test());
        assert!(!Environment::Production.is_any_test());
        assert!(!Environment::Development.is_any_test());
    }
}
