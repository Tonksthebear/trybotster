//! Cache-path and Zig version-gate tests for the Ghostty build script helpers.

#[allow(dead_code)]
#[path = "../build_support.rs"]
mod build_support;

use std::path::{Path, PathBuf};

use build_support::{
    direct_zig, resolve_zig_command, zig_candidates, zig_global_cache_dir, zig_local_cache_dir,
    REQUIRED_ZIG_VERSION,
};

#[test]
fn default_zig_caches_share_the_cargo_out_dir() {
    let out_dir = Path::new("target/build/botster-ghostty/out");

    assert_eq!(
        Path::new(&zig_local_cache_dir(
            out_dir.to_str().expect("test path is UTF-8")
        )),
        out_dir.join("zig-local-cache")
    );
    assert_eq!(
        Path::new(&zig_global_cache_dir(
            out_dir.to_str().expect("test path is UTF-8"),
            None
        )),
        out_dir.join("zig-global-cache")
    );
}

#[test]
fn resolution_requires_the_exact_pinned_zig_version() {
    let candidates = [
        direct_zig("/opt/zig-old/zig", "old"),
        direct_zig("/opt/zig-pinned/zig", "pinned"),
    ];

    let resolved = resolve_zig_command(&candidates, |candidate| match candidate.label.as_str() {
        "old" => Ok("0.15.2".to_owned()),
        _ => Ok(REQUIRED_ZIG_VERSION.to_owned()),
    })
    .expect("pinned Zig candidate resolves");

    assert_eq!(resolved.label, "pinned");
}

#[test]
fn resolution_fails_when_no_candidate_matches_the_pinned_version() {
    let candidates = [direct_zig("/opt/zig-old/zig", "old")];

    let error = resolve_zig_command(&candidates, |_| Ok("0.15.2".to_owned()))
        .expect_err("mismatched Zig must not resolve");

    assert!(
        error.contains(REQUIRED_ZIG_VERSION),
        "error must name the required version, got: {error}"
    );
    assert!(
        error.contains("0.15.2"),
        "error must name the rejected version, got: {error}"
    );
}

#[test]
fn mise_install_candidate_tracks_the_pinned_version() {
    let candidates = zig_candidates(None, None, Some("/home/agent".to_owned()), |path| {
        path == &PathBuf::from(format!(
            "/home/agent/.local/share/mise/installs/zig/{REQUIRED_ZIG_VERSION}/bin/zig"
        ))
    });

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.program.contains(REQUIRED_ZIG_VERSION)),
        "mise install candidate must follow the pinned version, got: {:?}",
        candidates
            .iter()
            .map(|candidate| candidate.program.clone())
            .collect::<Vec<_>>()
    );
}
