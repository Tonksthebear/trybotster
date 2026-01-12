//! Runtime environment detection.
//!
//! Provides a single source of truth for determining the runtime environment
//! (test, development, production) based on the `BOTSTER_ENV` environment variable.
//!
//! # Usage
//!
//! ```rust
//! use botster_hub::env::{Environment, is_test_mode};
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
}

impl Environment {
    /// Detect current environment from `BOTSTER_ENV`.
    ///
    /// Returns `Test` if `BOTSTER_ENV=test`, `Development` if `BOTSTER_ENV=development`
    /// or `BOTSTER_ENV=dev`, otherwise `Production`.
    #[must_use]
    pub fn current() -> Self {
        match std::env::var("BOTSTER_ENV").as_deref() {
            Ok("test") => Self::Test,
            Ok("development") | Ok("dev") => Self::Development,
            _ => Self::Production,
        }
    }

    /// Returns `true` if this is the test environment.
    #[must_use]
    pub fn is_test(self) -> bool {
        self == Self::Test
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
        }
    }
}

/// Convenience function to check if running in test mode.
///
/// Equivalent to `Environment::current().is_test()`.
#[must_use]
pub fn is_test_mode() -> bool {
    Environment::current().is_test()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_display() {
        assert_eq!(Environment::Production.to_string(), "production");
        assert_eq!(Environment::Development.to_string(), "development");
        assert_eq!(Environment::Test.to_string(), "test");
    }

    #[test]
    fn test_environment_is_methods() {
        assert!(Environment::Test.is_test());
        assert!(!Environment::Test.is_production());
        assert!(!Environment::Test.is_development());

        assert!(Environment::Production.is_production());
        assert!(!Environment::Production.is_test());

        assert!(Environment::Development.is_development());
        assert!(!Environment::Development.is_test());
    }
}
