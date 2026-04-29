//! Integration coverage for the cli/test.sh wrapper.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn check_mode_exports_test_env_and_mise_zig_without_real_cargo() {
    let result = run_check_with_fake_cargo(None);

    assert!(
        result.output.status.success(),
        "{}",
        result.failure_message()
    );
    assert!(result.captured_env.contains("BOTSTER_ENV=test\n"));
    assert!(result
        .captured_env
        .contains(&format!("BOTSTER_ZIG={}\n", result.mise_zig_path.display())));
    assert_eq!(result.captured_args, "check\n");
}

#[test]
fn check_mode_preserves_existing_botster_zig_without_real_cargo() {
    let configured_zig = "/opt/botster/custom-zig";
    let result = run_check_with_fake_cargo(Some(configured_zig));

    assert!(
        result.output.status.success(),
        "{}",
        result.failure_message()
    );
    assert!(result.captured_env.contains("BOTSTER_ENV=test\n"));
    assert!(result
        .captured_env
        .contains(&format!("BOTSTER_ZIG={configured_zig}\n")));
    assert_eq!(result.captured_args, "check\n");
}

struct FakeCargoRun {
    output: std::process::Output,
    captured_env: String,
    captured_args: String,
    mise_zig_path: std::path::PathBuf,
}

impl FakeCargoRun {
    fn failure_message(&self) -> String {
        format!(
            "test.sh --check failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&self.output.stdout),
            String::from_utf8_lossy(&self.output.stderr)
        )
    }
}

fn run_check_with_fake_cargo(botster_zig: Option<&str>) -> FakeCargoRun {
    let temp_dir = TempDir::new().expect("create temp dir");
    let home_dir = temp_dir.path().join("home");
    let fake_bin_dir = temp_dir.path().join("bin");
    let env_capture = temp_dir.path().join("cargo.env");
    let args_capture = temp_dir.path().join("cargo.args");
    let zig_path = home_dir.join(".local/share/mise/installs/zig/0.15.2/bin/zig");
    let fake_cargo_path = fake_bin_dir.join("cargo");

    fs::create_dir_all(zig_path.parent().expect("zig parent")).expect("create zig parent");
    fs::create_dir_all(&fake_bin_dir).expect("create fake bin dir");

    fs::write(&zig_path, "#!/bin/sh\nexit 0\n").expect("write fake zig");
    make_executable(&zig_path);

    fs::write(
        &fake_cargo_path,
        format!(
            "#!/bin/sh\nprintf 'BOTSTER_ENV=%s\\nBOTSTER_ZIG=%s\\n' \"$BOTSTER_ENV\" \"$BOTSTER_ZIG\" > {}\nprintf '%s\\n' \"$*\" > {}\nexit 0\n",
            shell_quote(env_capture.to_string_lossy().as_ref()),
            shell_quote(args_capture.to_string_lossy().as_ref())
        ),
    )
    .expect("write fake cargo");
    make_executable(&fake_cargo_path);

    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut command = Command::new("bash");
    command
        .args(["./test.sh", "--check"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("HOME", &home_dir)
        .env("PATH", path)
        .env_remove("BOTSTER_ENV")
        .env_remove("BOTSTER_ZIG");
    if let Some(botster_zig) = botster_zig {
        command.env("BOTSTER_ZIG", botster_zig);
    }

    FakeCargoRun {
        output: command.output().expect("run test.sh --check"),
        captured_env: fs::read_to_string(&env_capture).expect("read captured cargo env"),
        captured_args: fs::read_to_string(&args_capture).expect("read captured cargo args"),
        mise_zig_path: zig_path,
    }
}

fn make_executable(path: &std::path::Path) {
    let mut permissions = fs::metadata(path).expect("read metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable bit");
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
