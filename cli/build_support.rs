use std::path::PathBuf;

/// Exact Zig toolchain required to build the vendored Ghostty pin.
///
/// Upstream Ghostty declares `minimum_zig_version = "0.16.0"` at
/// `22d13172cde98a0a4dda05d3d6a3fcb0dd8ed018`. A near-miss toolchain fails
/// deep inside the Zig build, so the preflight gate requires this exact
/// version string.
pub const REQUIRED_ZIG_VERSION: &str = "0.16.0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZigCommand {
    pub program: String,
    pub prefix_args: Vec<String>,
    pub label: String,
}

pub fn direct_zig(path: impl Into<String>, label: impl Into<String>) -> ZigCommand {
    ZigCommand {
        program: path.into(),
        prefix_args: Vec::new(),
        label: label.into(),
    }
}

pub fn mise_zig() -> ZigCommand {
    ZigCommand {
        program: "mise".into(),
        prefix_args: vec!["exec".into(), "--".into(), "zig".into()],
        label: "mise exec -- zig".into(),
    }
}

pub fn zig_candidates(
    botster_zig: Option<String>,
    zig: Option<String>,
    home: Option<String>,
    path_exists: impl Fn(&PathBuf) -> bool,
) -> Vec<ZigCommand> {
    let mut candidates = Vec::new();

    if let Some(path) = botster_zig {
        candidates.push(direct_zig(path, "BOTSTER_ZIG"));
    }

    if let Some(path) = zig {
        candidates.push(direct_zig(path, "ZIG"));
    }

    if let Some(home) = home {
        // Keep the hardcoded mise install path in lockstep with REQUIRED_ZIG_VERSION.
        let mise_zig =
            PathBuf::from(home).join(format!(
                ".local/share/mise/installs/zig/{REQUIRED_ZIG_VERSION}/bin/zig"
            ));
        if path_exists(&mise_zig) {
            candidates.push(direct_zig(
                mise_zig.display().to_string(),
                format!("mise Zig {REQUIRED_ZIG_VERSION} install"),
            ));
        }
    }

    candidates.push(direct_zig("zig", "zig from PATH"));
    candidates.push(mise_zig());

    candidates
}

pub fn resolve_zig_command(
    candidates: &[ZigCommand],
    mut version: impl FnMut(&ZigCommand) -> Result<String, String>,
) -> Result<ZigCommand, String> {
    let mut errors = Vec::new();

    for candidate in candidates {
        match version(candidate) {
            Ok(found) if found == REQUIRED_ZIG_VERSION => return Ok(candidate.clone()),
            Ok(found) => errors.push(format!(
                "Skipping {}: Zig {found} found, but Botster requires Zig {REQUIRED_ZIG_VERSION}",
                candidate.label
            )),
            Err(err) => errors.push(format!("Skipping {}: {err}", candidate.label)),
        }
    }

    Err(format!(
        "Botster requires Zig {REQUIRED_ZIG_VERSION} to build vendored Ghostty. \
         Set BOTSTER_ZIG to a Zig {REQUIRED_ZIG_VERSION} binary, or install it with mise.\n{}",
        errors.join("\n")
    ))
}

pub fn zig_global_cache_dir(out_dir: &str, configured: Option<String>) -> String {
    configured.unwrap_or_else(|| {
        PathBuf::from(out_dir)
            .join("zig-global-cache")
            .display()
            .to_string()
    })
}

/// Local Zig cache scoped to Cargo's OUT_DIR so builds do not write `.zig-cache`
/// into the shared vendored Ghostty checkout.
pub fn zig_local_cache_dir(out_dir: &str) -> String {
    PathBuf::from(out_dir)
        .join("zig-local-cache")
        .display()
        .to_string()
}
