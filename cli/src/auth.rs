//! Device authorization flow for CLI authentication.
//!
//! Implements RFC 8628 (OAuth 2.0 Device Authorization Grant) to authenticate
//! the CLI with the server without requiring manual API key configuration.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// Response from POST /api/device_codes
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

/// Successful token response from GET /api/device_codes/:device_code
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    /// The access token for API authentication.
    pub access_token: String,
    /// Token type (typically "Bearer").
    pub token_type: String,
}

/// Error response during polling
#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    /// Error code (e.g., "authorization_pending", "slow_down").
    pub error: String,
}

/// Perform device authorization flow to obtain an access token.
///
/// This function will:
/// 1. Request a device code from the server
/// 2. Display the verification URL and user code to the user
/// 3. Optionally open the browser (unless BOTSTER_NO_BROWSER is set)
/// 4. Poll the server until the user approves or the code expires
/// 5. Return the access token on success
pub fn device_flow(server_url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // Get device name from hostname
    let device_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Botster CLI".to_string());

    // Step 1: Request device code
    let url = format!("{}/api/device_codes", server_url);
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

    let device_code: DeviceCodeResponse = response.json().context("Invalid device code response")?;

    // Step 2: Display instructions to user
    println!();
    println!("  To authenticate, visit:");
    println!();
    println!("    {}", device_code.verification_uri);
    println!();
    println!("  And enter this code:");
    println!();
    println!("    {}", device_code.user_code);
    println!();

    // Check if we're in interactive mode (TTY)
    let interactive = atty::is(atty::Stream::Stdin)
        && std::env::var("BOTSTER_NO_BROWSER").is_err()
        && std::env::var("CI").is_err();

    // Spawn a thread to listen for Enter key to open browser
    let verification_uri = device_code.verification_uri.clone();
    let browser_thread = if interactive {
        println!("  Press Enter to open browser...");
        println!();
        Some(thread::spawn(move || {
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                match open_browser(&verification_uri) {
                    Ok(()) => println!("\r  Browser opened.                    "),
                    Err(e) => println!("\r  Could not open browser: {}         ", e),
                }
            }
        }))
    } else {
        println!("  Waiting for authorization...");
        println!();
        None
    };

    print!("  Polling");
    io::stdout().flush()?;

    // Step 3: Poll for authorization
    let poll_url = format!("{}/api/device_codes/{}", server_url, device_code.device_code);
    let poll_interval = Duration::from_secs(device_code.interval.max(5));
    let max_attempts = device_code.expires_in / device_code.interval.max(5);

    for attempt in 0..max_attempts {
        thread::sleep(poll_interval);

        let response = client
            .get(&poll_url)
            .send()
            .context("Failed to poll for authorization")?;

        let status = response.status();

        match status.as_u16() {
            200 => {
                // Success - we got the token
                let token: TokenResponse =
                    response.json().context("Invalid token response")?;
                println!();
                println!();
                println!("  Authorized successfully!");
                println!();
                // Browser thread will be dropped/ignored
                drop(browser_thread);
                return Ok(token.access_token);
            }
            202 => {
                // Still pending - continue polling
                print!(".");
                io::stdout().flush()?;
                continue;
            }
            400 | 401 | 403 => {
                // Check error type
                let error: ErrorResponse = response
                    .json()
                    .unwrap_or(ErrorResponse { error: "unknown".to_string() });

                match error.error.as_str() {
                    "authorization_pending" => {
                        // Shouldn't happen with 400, but handle it
                        print!(".");
                        io::stdout().flush()?;
                        continue;
                    }
                    "expired_token" => {
                        println!();
                        drop(browser_thread);
                        anyhow::bail!("Authorization code expired. Please try again.");
                    }
                    "access_denied" => {
                        println!();
                        drop(browser_thread);
                        anyhow::bail!("Authorization was denied.");
                    }
                    _ => {
                        println!();
                        drop(browser_thread);
                        anyhow::bail!("Authorization failed: {}", error.error);
                    }
                }
            }
            _ => {
                log::warn!(
                    "Unexpected status {} on poll attempt {}, retrying...",
                    status,
                    attempt
                );
                print!(".");
                io::stdout().flush()?;
                continue;
            }
        }
    }

    println!();
    drop(browser_thread);
    anyhow::bail!("Authorization timed out. Please try again.")
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
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            println!("  Token validation: failed to create HTTP client: {}", e);
            return false;
        }
    };

    // Try to list devices - a simple authenticated endpoint
    let url = format!("{}/api/devices", server_url);
    println!("  Validating token against {}...", url);

    match client.get(&url).header("X-API-Key", token).send() {
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
    }
}
