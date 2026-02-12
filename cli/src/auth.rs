//! Device authorization flow for CLI authentication.
//!
//! Implements RFC 8628 (OAuth 2.0 Device Authorization Grant) to authenticate
//! the CLI with the server without requiring manual API key configuration.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// Response from POST /hubs/codes
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    /// Opaque code for polling.
    pub device_code: String,
    /// Human-readable code to display to user.
    pub user_code: String,
    /// URL where user should enter the code.
    pub verification_uri: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Minimum polling interval in seconds.
    pub interval: u64,
}

/// Successful token response from GET /hubs/codes/:id
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    /// The access token for hub-server authentication (btstr_...).
    pub access_token: String,
    /// Token type (typically "Bearer").
    pub token_type: String,
    /// Optional MCP token for agent authentication (btmcp_...).
    /// Scoped to MCP operations only, passed to spawned agents.
    #[serde(default)]
    pub mcp_token: Option<String>,
}

/// Error response during polling
#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    /// Error code (e.g., "authorization_pending", "slow_down").
    pub error: String,
}

/// Perform device authorization flow to obtain access tokens.
///
/// This function will:
/// 1. Request a device code from the server
/// 2. Display the verification URL and user code to the user
/// 3. Optionally open the browser (unless BOTSTER_NO_BROWSER is set)
/// 4. Poll the server until the user approves or the code expires
/// 5. Return the token response containing both hub and MCP tokens
pub fn device_flow(server_url: &str) -> Result<TokenResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(crate::constants::user_agent())
        .build()?;

    // Get device name from hostname
    let device_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Botster CLI".to_string());

    // Step 1: Request device code
    let url = format!("{}/hubs/codes", server_url);
    let response = client
        .post(&url)
        .json(&serde_json::json!({ "device_name": device_name }))
        .send()
        .context("Failed to request device code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("Server returned {}: {}", status, body);
    }

    let device_code: DeviceCodeResponse =
        response.json().context("Invalid device code response")?;

    // Step 2: Display instructions
    println!();
    println!("  To authorize, visit:");
    println!();
    println!("    {}", device_code.verification_uri);
    println!();
    println!("  Code: {}", device_code.user_code);
    println!();

    // Step 3: Start polling in background thread immediately.
    // This allows the server to detect approval even before the user presses Enter,
    // which is critical for headless servers where there's no browser to open.
    let poll_url = format!("{}/hubs/codes/{}", server_url, device_code.device_code);
    let poll_interval = Duration::from_secs(device_code.interval.max(5));
    let max_attempts = device_code.expires_in / device_code.interval.max(5);

    let (tx, rx) = std::sync::mpsc::channel::<Result<TokenResponse, String>>();
    let poll_client = client.clone();
    let poll_url_clone = poll_url.clone();

    thread::spawn(move || {
        for attempt in 0..max_attempts {
            thread::sleep(poll_interval);

            let response = match poll_client.get(&poll_url_clone).send() {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("Poll attempt {} failed: {}", attempt + 1, e);
                    continue;
                }
            };

            match response.status().as_u16() {
                200 => {
                    match response.json::<TokenResponse>() {
                        Ok(token) => {
                            let _ = tx.send(Ok(token));
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(Err(format!("Invalid token response: {e}")));
                            return;
                        }
                    }
                }
                202 => continue, // Still pending
                400 | 401 | 403 => {
                    let error: ErrorResponse = response.json().unwrap_or(ErrorResponse {
                        error: "unknown".to_string(),
                    });
                    match error.error.as_str() {
                        "authorization_pending" => continue,
                        other => {
                            let _ = tx.send(Err(format!("Authorization failed: {other}")));
                            return;
                        }
                    }
                }
                status => {
                    log::warn!("Unexpected status {} on poll attempt {}", status, attempt + 1);
                    continue;
                }
            }
        }
        let _ = tx.send(Err("Authorization timed out. Please try again.".to_string()));
    });

    // Step 4: Show browser prompt (interactive) while polling runs in background
    let interactive = atty::is(atty::Stream::Stdin)
        && std::env::var("BOTSTER_NO_BROWSER").is_err()
        && std::env::var("CI").is_err();

    if interactive {
        println!("  Press Enter to open browser (or visit the URL above)...");
        io::stdout().flush()?;

        // Wait for either Enter key or poll result, whichever comes first
        let stdin_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stdin_flag = std::sync::Arc::clone(&stdin_done);

        // Read stdin in a separate thread so we can check poll results concurrently
        thread::spawn(move || {
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);
            stdin_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        // Check for Enter or poll result
        let mut browser_opened = false;
        loop {
            // Check if polling thread got a result
            if let Ok(result) = rx.try_recv() {
                return handle_poll_result(result);
            }

            // Check if user pressed Enter
            if !browser_opened && stdin_done.load(std::sync::atomic::Ordering::Relaxed) {
                browser_opened = true;
                match open_browser(&device_code.verification_uri) {
                    Ok(()) => println!("  Browser opened."),
                    Err(e) => println!("  Could not open browser: {}", e),
                }
                print!("  Waiting for approval");
                io::stdout().flush()?;
            }

            thread::sleep(Duration::from_millis(100));
        }
    }

    // Non-interactive: just wait for the polling thread result
    print!("  Waiting for approval");
    io::stdout().flush()?;

    loop {
        match rx.try_recv() {
            Ok(result) => return handle_poll_result(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                print!(".");
                io::stdout().flush()?;
                thread::sleep(poll_interval);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                println!();
                anyhow::bail!("Authorization polling thread terminated unexpectedly");
            }
        }
    }
}

/// Prompt the user to name their hub during first-time setup.
///
/// Detects the current git repo name as a default. Returns the chosen name.
pub fn prompt_hub_name() -> Result<String> {
    let repo_name = std::env::var("BOTSTER_REPO")
        .ok()
        .or_else(|| {
            crate::git::WorktreeManager::detect_current_repo()
                .map(|(_, name)| name)
                .ok()
        });

    println!();
    println!("  Setting up a new Botster hub.");
    println!();

    let default_name = repo_name.as_deref().unwrap_or("my-hub");

    if repo_name.is_some() {
        print!("  Name this hub (Enter for \"{}\"): ", default_name);
    } else {
        print!("  Name this hub: ");
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    let name = if input.is_empty() {
        default_name.to_string()
    } else {
        input.to_string()
    };

    println!();
    Ok(name)
}

/// Process the result from the background polling thread.
fn handle_poll_result(result: Result<TokenResponse, String>) -> Result<TokenResponse> {
    match result {
        Ok(token) => {
            println!();
            println!();
            println!("  Authorized successfully!");
            println!();
            Ok(token)
        }
        Err(msg) => {
            println!();
            anyhow::bail!("{msg}")
        }
    }
}

/// Try to open the verification URL in the user's browser.
fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("Failed to open browser")?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("Failed to open browser")?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .context("Failed to open browser")?;
    }

    Ok(())
}

/// Validate that a token is still valid by making a test API request.
/// Returns true only if we get a successful response from an authenticated endpoint.
pub fn validate_token(server_url: &str, token: &str) -> bool {
    if token.is_empty() {
        println!("  Token validation: empty token");
        return false;
    }

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(crate::constants::user_agent())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            println!("  Token validation: failed to create HTTP client: {}", e);
            return false;
        }
    };

    // Try to list devices - a simple authenticated endpoint
    let url = format!("{}/devices", server_url);
    println!("  Validating token against {}...", url);

    match client.get(&url).bearer_auth(token).send() {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                println!("  Token valid (status: {})", status);
                true
            } else {
                println!("  Token invalid (status: {})", status);
                false
            }
        }
        Err(e) => {
            // Network error - could be server down, but we treat as "needs re-auth"
            // to be safe. User can skip validation with env var if needed.
            println!("  Token validation failed: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_code_response_deserialize() {
        let json = r#"{
            "device_code": "abc123",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://example.com/device",
            "expires_in": 900,
            "interval": 5
        }"#;
        let resp: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.device_code, "abc123");
        assert_eq!(resp.user_code, "WDJB-MJHT");
        assert_eq!(resp.expires_in, 900);
    }

    #[test]
    fn test_token_response_deserialize() {
        let json = r#"{
            "access_token": "btstr_xyz789",
            "token_type": "bearer"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "btstr_xyz789");
        assert_eq!(resp.token_type, "bearer");
        assert_eq!(resp.mcp_token, None);
    }

    #[test]
    fn test_token_response_with_mcp_token() {
        let json = r#"{
            "access_token": "btstr_hub123",
            "token_type": "bearer",
            "mcp_token": "btmcp_agent456"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "btstr_hub123");
        assert_eq!(resp.mcp_token, Some("btmcp_agent456".to_string()));
    }

    #[test]
    fn test_token_response_mcp_token_optional() {
        // Old servers might not return mcp_token - should still work
        let json = r#"{
            "access_token": "btstr_old",
            "token_type": "bearer"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "btstr_old");
        assert!(resp.mcp_token.is_none());
    }
}
