use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// Spawn a command in an external terminal window
/// Returns the window ID (macOS only) for focusing later
/// This is the proper way to run TUI applications like Claude Code
pub fn spawn_in_external_terminal(
    command: &str,
    worktree_path: &Path,
    title: &str,
) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        spawn_in_terminal_macos(command, worktree_path, title)
    }

    #[cfg(target_os = "linux")]
    {
        spawn_in_terminal_linux(command, worktree_path, title)?;
        Ok(None)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("External terminal spawning not supported on this platform")
    }
}

#[cfg(target_os = "macos")]
fn spawn_in_terminal_macos(
    command: &str,
    worktree_path: &Path,
    title: &str,
) -> Result<Option<String>> {
    // Try iTerm first, fall back to Terminal.app
    if is_iterm_available() {
        spawn_in_iterm(command, worktree_path, title)
    } else {
        spawn_in_terminal_app(command, worktree_path, title)
    }
}

#[cfg(target_os = "macos")]
fn is_iterm_available() -> bool {
    Command::new("osascript")
        .args(&["-e", "application \"iTerm\" is running"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn spawn_in_iterm(command: &str, worktree_path: &Path, title: &str) -> Result<Option<String>> {
    // Escape special characters for AppleScript
    let escaped_command = command.replace("\\", "\\\\").replace("\"", "\\\"");
    let escaped_title = title.replace("\\", "\\\\").replace("\"", "\\\"");

    let script = format!(
        r#"
        tell application "iTerm"
            create window with default profile
            tell current session of current window
                set name to "{}"
                write text "cd {}"
                write text "{}"
            end tell
        end tell
        "#,
        escaped_title,
        worktree_path.display(),
        escaped_command
    );

    Command::new("osascript").args(&["-e", &script]).spawn()?;

    Ok(None)
}

#[cfg(target_os = "macos")]
fn spawn_in_terminal_app(
    command: &str,
    worktree_path: &Path,
    _title: &str,
) -> Result<Option<String>> {
    // Escape special characters for AppleScript
    let escaped_command = command.replace("\\", "\\\\").replace("\"", "\\\"");

    let script = format!(
        r#"tell application "Terminal" to do script "cd {} && {}""#,
        worktree_path.display(),
        escaped_command
    );

    let output = Command::new("osascript").args(&["-e", &script]).output()?;

    // The output is like "tab 1 of window id 26078"
    let window_info = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Extract window ID from "tab 1 of window id 26078"
    let window_id = window_info
        .split("window id ")
        .nth(1)
        .map(|s| s.trim().to_string());

    Ok(window_id)
}

#[cfg(target_os = "linux")]
fn spawn_in_terminal_linux(command: &str, worktree_path: &Path, title: &str) -> Result<()> {
    // Try common Linux terminals in order of preference
    let terminals = vec![
        ("gnome-terminal", vec!["--title", title, "--", "bash", "-c"]),
        ("konsole", vec!["--title", title, "-e", "bash", "-c"]),
        ("xterm", vec!["-T", title, "-e", "bash", "-c"]),
        ("alacritty", vec!["--title", title, "-e", "bash", "-c"]),
    ];

    let full_command = format!("cd {} && {}", worktree_path.display(), command);

    for (terminal, mut args) in terminals {
        if Command::new("which")
            .arg(terminal)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            args.push(&full_command);
            Command::new(terminal).args(&args).spawn()?;
            return Ok(());
        }
    }

    anyhow::bail!("No supported terminal emulator found")
}

#[cfg(test)]
mod tests {

    use tempfile::TempDir;

    #[test]
    #[cfg(target_os = "macos")]
    fn test_terminal_spawning() {
        let _temp_dir = TempDir::new().unwrap();

        // This will actually spawn a terminal window - only run manually
        // spawn_in_external_terminal("echo 'Test'", temp_dir.path(), "Test").unwrap();
    }
}
