//! Focused tests for the build-script Zig resolver support.

#[path = "../build_support.rs"]
mod build_support;

use std::path::PathBuf;

use build_support::{
    direct_zig, mise_zig, resolve_zig_command, zig_candidates, zig_global_cache_dir,
};

#[test]
fn zig_resolver_orders_candidates_from_explicit_to_fallback() {
    let candidates = zig_candidates(
        Some("/opt/botster/zig".to_string()),
        Some("/opt/env/zig".to_string()),
        Some("/home/example".to_string()),
        |path| {
            path == &PathBuf::from("/home/example/.local/share/mise/installs/zig/0.15.2/bin/zig")
        },
    );

    let labels: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "BOTSTER_ZIG",
            "ZIG",
            "mise Zig 0.15.2 install",
            "zig from PATH",
            "mise exec -- zig",
        ]
    );

    assert_eq!(candidates[0].program, "/opt/botster/zig");
    assert_eq!(candidates[1].program, "/opt/env/zig");
    assert_eq!(
        candidates[2].program,
        "/home/example/.local/share/mise/installs/zig/0.15.2/bin/zig"
    );
    assert_eq!(candidates[3].program, "zig");
    assert_eq!(candidates[4], mise_zig());
}

#[test]
fn zig_resolver_skips_missing_local_mise_install_when_ordering_candidates() {
    let candidates = zig_candidates(None, None, Some("/home/example".to_string()), |_| false);

    assert_eq!(candidates, [direct_zig("zig", "zig from PATH"), mise_zig()]);
}

#[test]
fn zig_resolver_selects_first_candidate_reporting_required_zig_version() {
    let candidates = vec![
        direct_zig("/opt/wrong/zig", "wrong version"),
        direct_zig("/opt/broken/zig", "broken command"),
        direct_zig("/opt/good/zig", "good command"),
        direct_zig("/opt/later/zig", "later command"),
    ];

    let mut checked = Vec::new();
    let selected = resolve_zig_command(&candidates, |candidate| {
        checked.push(candidate.label.clone());
        match candidate.label.as_str() {
            "wrong version" => Ok("0.15.1".to_string()),
            "broken command" => Err("permission denied".to_string()),
            "good command" => Ok("0.15.2".to_string()),
            label => panic!("unexpected candidate checked: {label}"),
        }
    })
    .expect("expected compatible Zig candidate");

    assert_eq!(selected, direct_zig("/opt/good/zig", "good command"));
    assert_eq!(checked, ["wrong version", "broken command", "good command"]);
}

#[test]
fn zig_resolver_returns_useful_failure_text_when_no_candidate_is_compatible() {
    let candidates = vec![
        direct_zig("/opt/wrong/zig", "wrong version"),
        direct_zig("/opt/broken/zig", "broken command"),
    ];

    let err = resolve_zig_command(&candidates, |candidate| match candidate.label.as_str() {
        "wrong version" => Ok("0.14.1".to_string()),
        "broken command" => Err("No such file or directory".to_string()),
        label => panic!("unexpected candidate checked: {label}"),
    })
    .expect_err("expected resolver failure");

    assert!(err.contains("Botster requires Zig 0.15.2 to build vendored Ghostty"));
    assert!(err.contains("Set BOTSTER_ZIG to a Zig 0.15.2 binary"));
    assert!(err.contains("install it with mise"));
    assert!(
        err.contains("Skipping wrong version: Zig 0.14.1 found, but Botster requires Zig 0.15.2")
    );
    assert!(err.contains("Skipping broken command: No such file or directory"));
}

#[test]
fn zig_resolver_global_cache_dir_honors_env_before_defaulting_under_out_dir() {
    assert_eq!(
        zig_global_cache_dir("/tmp/cargo-out", Some("/tmp/custom-zig-cache".to_string())),
        "/tmp/custom-zig-cache"
    );
    assert_eq!(
        zig_global_cache_dir("/tmp/cargo-out", None),
        "/tmp/cargo-out/zig-global-cache"
    );
}
