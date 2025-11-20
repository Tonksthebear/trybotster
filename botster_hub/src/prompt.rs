use anyhow::{Context, Result};
use std::path::Path;

const DEFAULT_PROMPT_REPO: &str = "Tonksthebear/trybotster";
const DEFAULT_PROMPT_PATH: &str = "botster_hub/botster_prompt";

pub struct PromptManager;

impl PromptManager {
    /// Get the prompt for a worktree
    /// Priority: local .botster_prompt > remote botster_hub/botster_prompt.*
    pub fn get_prompt(worktree_path: &Path) -> Result<String> {
        // 1. Check for local .botster_prompt file
        let local_prompt_path = worktree_path.join(".botster_prompt");
        if local_prompt_path.exists() {
            log::info!("Using local prompt from .botster_prompt");
            return std::fs::read_to_string(&local_prompt_path)
                .context("Failed to read .botster_prompt");
        }

        // 2. Fetch from GitHub (botster_hub/botster_prompt.* with any extension)
        log::info!("Fetching default prompt from {}", DEFAULT_PROMPT_REPO);
        Self::fetch_default_prompt()
    }

    fn fetch_default_prompt() -> Result<String> {
        // Try common extensions in order
        let extensions = ["md", "txt", ""];

        for ext in &extensions {
            let filename = if ext.is_empty() {
                DEFAULT_PROMPT_PATH.to_string()
            } else {
                format!("{}.{}", DEFAULT_PROMPT_PATH, ext)
            };

            let url = format!(
                "https://raw.githubusercontent.com/{}/main/{}",
                DEFAULT_PROMPT_REPO,
                filename
            );

            log::debug!("Trying to fetch prompt from: {}", url);

            match reqwest::blocking::get(&url) {
                Ok(response) if response.status().is_success() => {
                    log::info!("Found prompt at: {}", filename);
                    return response.text().context("Failed to read prompt text");
                }
                Ok(response) => {
                    log::debug!("Got status {} for {}", response.status(), filename);
                }
                Err(e) => {
                    log::debug!("Error fetching {}: {}", filename, e);
                }
            }
        }

        anyhow::bail!(
            "Could not find prompt file at {}. Tried extensions: {:?}",
            DEFAULT_PROMPT_PATH,
            extensions
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_local_prompt_takes_priority() {
        let temp_dir = TempDir::new().unwrap();
        let prompt_path = temp_dir.path().join(".botster_prompt");
        std::fs::write(&prompt_path, "Local test prompt").unwrap();

        let result = PromptManager::get_prompt(temp_dir.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Local test prompt");
    }

    #[test]
    fn test_fetch_default_prompt() {
        // This test requires network access
        // Skip in CI or when offline
        if std::env::var("SKIP_NETWORK_TESTS").is_ok() {
            return;
        }

        let result = PromptManager::fetch_default_prompt();
        if result.is_ok() {
            let prompt = result.unwrap();
            assert!(!prompt.is_empty(), "Prompt should not be empty");
        }
        // Don't fail the test if GitHub is unreachable
    }
}
