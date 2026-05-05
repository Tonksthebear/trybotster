use super::{detect_running_cargo_profile, detect_running_target_dir, CargoBuildProfile};
use std::path::Path;

#[test]
pub(super) fn detects_debug_profile_from_target_path() {
    let exe = Path::new("/repo/target/debug/botster");
    assert_eq!(
        detect_running_cargo_profile(exe),
        Some(CargoBuildProfile::Debug)
    );
}

#[test]
pub(super) fn detects_release_profile_from_target_path() {
    let exe = Path::new("/repo/target/release/botster");
    assert_eq!(
        detect_running_cargo_profile(exe),
        Some(CargoBuildProfile::Release)
    );
}

#[test]
pub(super) fn detects_named_profile_from_target_path() {
    let exe = Path::new("/repo/target/profiling/botster");
    assert_eq!(
        detect_running_cargo_profile(exe),
        Some(CargoBuildProfile::Named("profiling".to_string()))
    );
}

#[test]
pub(super) fn returns_none_outside_cargo_target_tree() {
    let exe = Path::new("/usr/local/bin/botster");
    assert_eq!(detect_running_cargo_profile(exe), None);
}

#[test]
pub(super) fn detects_target_dir_from_target_tree_path() {
    let exe = Path::new("/repo/target/debug/botster");
    assert_eq!(
        detect_running_target_dir(exe),
        Some(Path::new("/repo/target").to_path_buf())
    );
}

#[test]
pub(super) fn target_dir_none_outside_target_tree() {
    let exe = Path::new("/usr/local/bin/botster");
    assert_eq!(detect_running_target_dir(exe), None);
}
