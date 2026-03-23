//! Device-scoped spawn target persistence and inspection.
//!
//! Spawn targets are explicitly admitted filesystem roots where the hub is
//! allowed to spawn sessions. Authorization is based on the admitted path
//! itself; git state is derived fresh from the filesystem at inspection time.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

use crate::Config;

/// Persisted spawn target admission record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnTarget {
    /// Stable spawn target identifier.
    pub id: String,
    /// User-visible display name.
    pub name: String,
    /// Canonical filesystem root for the admitted target.
    pub path: String,
    /// Whether the target can be used for new spawns.
    pub enabled: bool,
    /// Creation timestamp in RFC 3339 form.
    pub created_at: String,
    /// Last update timestamp in RFC 3339 form.
    pub updated_at: String,
    /// Optional plugin whitelist for sessions in this target.
    /// `None` means no plugins (deny-by-default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<String>>,
}

/// Live inspection result for a target or candidate directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnTargetInspection {
    /// Canonical or normalized absolute path being inspected.
    pub path: String,
    /// Whether the path exists at inspection time.
    pub exists: bool,
    /// Whether the path is a directory at inspection time.
    pub is_directory: bool,
    /// Whether the path is already admitted as a spawn target.
    pub admitted: bool,
    /// Matching admitted target ID when present.
    pub admitted_target_id: Option<String>,
    /// Matching admitted target enabled flag when present.
    pub admitted_target_enabled: bool,
    /// Whether the path is currently inside a git repository.
    pub is_git_repo: bool,
    /// Live git repo root when git-backed.
    pub repo_root: Option<String>,
    /// Live repo name derived from origin or repo basename.
    pub repo_name: Option<String>,
    /// Live current branch when git-backed.
    pub current_branch: Option<String>,
    /// Live default branch when origin HEAD is configured.
    pub default_branch: Option<String>,
    /// Whether `{target}/.botster/` currently exists.
    pub has_botster_dir: bool,
    /// Whether git worktree operations currently succeed for the path.
    pub supports_worktrees: bool,
}

/// Filesystem-backed spawn target registry.
#[derive(Clone, Debug)]
pub struct SpawnTargetRegistry {
    path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSpawnTargets {
    targets: Vec<SpawnTarget>,
}

#[derive(Debug, Default)]
struct GitCapabilities {
    is_git_repo: bool,
    repo_root: Option<String>,
    repo_name: Option<String>,
    current_branch: Option<String>,
    default_branch: Option<String>,
    supports_worktrees: bool,
}

static SPAWN_TARGETS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl SpawnTargetRegistry {
    /// Create a registry backed by the default device-scoped storage path.
    pub fn load_default() -> Result<Self> {
        Ok(Self::new(Self::default_path()?))
    }

    /// Create a registry backed by a specific JSON file.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Return the default on-disk registry path.
    pub fn default_path() -> Result<PathBuf> {
        Ok(Config::config_dir()?.join("spawn_targets.json"))
    }

    /// List all admitted spawn targets.
    pub fn list(&self) -> Result<Vec<SpawnTarget>> {
        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        Ok(self.load_store()?.targets)
    }

    /// Fetch one admitted target by ID.
    pub fn get(&self, id: &str) -> Result<Option<SpawnTarget>> {
        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        Ok(self
            .load_store()?
            .targets
            .into_iter()
            .find(|target| target.id == id))
    }

    /// Admit a directory as a spawn target.
    ///
    /// Paths are canonicalized on write. Re-adding an existing canonical path
    /// is idempotent and re-enables the target.
    pub fn add<P: AsRef<Path>>(
        &self,
        path: P,
        name: Option<&str>,
        plugins: Option<Vec<String>>,
    ) -> Result<SpawnTarget> {
        let canonical = canonicalize_directory(path.as_ref())?;
        let canonical_path = path_string(&canonical);
        let now = Utc::now().to_rfc3339();

        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        let mut store = self.load_store()?;

        if let Some(existing) = store
            .targets
            .iter_mut()
            .find(|target| target.path == canonical_path)
        {
            if let Some(name) = normalize_name(name) {
                existing.name = name;
            }
            if let Some(plugins) = plugins {
                existing.plugins = Some(plugins);
            }
            existing.enabled = true;
            existing.updated_at = now;
            let target = existing.clone();
            self.save_store(&store)?;
            return Ok(target);
        }

        let target = SpawnTarget {
            id: format!("tgt_{}", Uuid::new_v4().simple()),
            name: normalize_name(name).unwrap_or_else(|| default_target_name(&canonical)),
            path: canonical_path,
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            plugins,
        };

        store.targets.push(target.clone());
        self.save_store(&store)?;
        Ok(target)
    }

    /// Update persisted fields for an admitted spawn target.
    pub fn update(
        &self,
        id: &str,
        name: Option<&str>,
        enabled: Option<bool>,
        plugins: Option<Vec<String>>,
    ) -> Result<Option<SpawnTarget>> {
        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        let mut store = self.load_store()?;

        let Some(index) = store.targets.iter().position(|target| target.id == id) else {
            return Ok(None);
        };

        let existing = store
            .targets
            .get_mut(index)
            .expect("target index should stay valid");
        let mut changed = false;
        if let Some(name) = normalize_name(name) {
            if existing.name != name {
                existing.name = name;
                changed = true;
            }
        }
        if let Some(enabled) = enabled {
            if existing.enabled != enabled {
                existing.enabled = enabled;
                changed = true;
            }
        }
        if let Some(plugins) = plugins {
            if existing.plugins.as_ref() != Some(&plugins) {
                existing.plugins = Some(plugins);
                changed = true;
            }
        }
        if changed {
            existing.updated_at = Utc::now().to_rfc3339();
        }
        let updated = existing.clone();
        if changed {
            self.save_store(&store)?;
        }

        Ok(Some(updated))
    }

    /// Enable or disable an admitted spawn target.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Option<SpawnTarget>> {
        self.update(id, None, Some(enabled), None)
    }

    /// Disable an admitted spawn target.
    pub fn disable(&self, id: &str) -> Result<Option<SpawnTarget>> {
        self.set_enabled(id, false)
    }

    /// Enable an admitted spawn target.
    pub fn enable(&self, id: &str) -> Result<Option<SpawnTarget>> {
        self.set_enabled(id, true)
    }

    /// Remove an admitted spawn target.
    pub fn remove(&self, id: &str) -> Result<Option<SpawnTarget>> {
        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        let mut store = self.load_store()?;
        let Some(index) = store.targets.iter().position(|target| target.id == id) else {
            return Ok(None);
        };
        let removed = store.targets.remove(index);

        self.save_store(&store)?;
        Ok(Some(removed))
    }

    /// Inspect a target or candidate directory without admitting it.
    pub fn inspect<P: AsRef<Path>>(&self, path: P) -> Result<SpawnTargetInspection> {
        let resolved = resolve_for_inspection(path.as_ref())?;
        let exists = resolved.exists();
        let is_directory = resolved.is_dir();
        let resolved_path = path_string(&resolved);

        let _guard = SPAWN_TARGETS_LOCK
            .lock()
            .expect("spawn target lock poisoned");
        let store = self.load_store()?;
        let admitted_target = store
            .targets
            .iter()
            .find(|target| target.path == resolved_path);

        let capabilities = if is_directory {
            GitCapabilities::inspect(&resolved)
        } else {
            GitCapabilities::default()
        };

        Ok(SpawnTargetInspection {
            path: resolved_path,
            exists,
            is_directory,
            admitted: admitted_target.is_some(),
            admitted_target_id: admitted_target.map(|target| target.id.clone()),
            admitted_target_enabled: admitted_target.is_some_and(|target| target.enabled),
            is_git_repo: capabilities.is_git_repo,
            repo_root: capabilities.repo_root,
            repo_name: capabilities.repo_name,
            current_branch: capabilities.current_branch,
            default_branch: capabilities.default_branch,
            has_botster_dir: is_directory && resolved.join(".botster").is_dir(),
            supports_worktrees: capabilities.supports_worktrees,
        })
    }

    fn load_store(&self) -> Result<StoredSpawnTargets> {
        if !self.path.exists() {
            return Ok(StoredSpawnTargets::default());
        }

        let content = fs::read_to_string(&self.path).with_context(|| {
            format!(
                "Failed to read spawn target registry {}",
                self.path.display()
            )
        })?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse spawn target registry {}",
                self.path.display()
            )
        })
    }

    fn save_store(&self, store: &StoredSpawnTargets) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create spawn target registry directory {}",
                    parent.display()
                )
            })?;
        }

        let content =
            serde_json::to_string_pretty(store).context("Failed to serialize spawn targets")?;
        fs::write(&self.path, content).with_context(|| {
            format!(
                "Failed to write spawn target registry {}",
                self.path.display()
            )
        })?;
        Ok(())
    }
}

impl GitCapabilities {
    fn inspect(path: &Path) -> Self {
        let Some(repo_root_raw) = git_stdout(path, ["rev-parse", "--show-toplevel"]) else {
            return Self::default();
        };

        let repo_root_path = PathBuf::from(repo_root_raw.trim());
        let repo_root = repo_root_path
            .canonicalize()
            .unwrap_or(repo_root_path.clone());
        let repo_root_string = path_string(&repo_root);

        let repo_name = git_stdout(path, ["remote", "get-url", "origin"])
            .and_then(|remote| repo_name_from_remote(&remote))
            .or_else(|| {
                repo_root
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });

        let current_branch = git_stdout(path, ["branch", "--show-current"]).and_then(non_empty);
        let default_branch = git_stdout(path, ["symbolic-ref", "refs/remotes/origin/HEAD"])
            .and_then(|head| {
                non_empty(
                    head.trim()
                        .trim_start_matches("refs/remotes/origin/")
                        .to_string(),
                )
            });
        let supports_worktrees = git_succeeds(path, ["worktree", "list", "--porcelain"]);

        Self {
            is_git_repo: true,
            repo_root: Some(repo_root_string),
            repo_name,
            current_branch,
            default_branch,
            supports_worktrees,
        }
    }
}

fn canonicalize_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve spawn target path {}", path.display()))?;

    if !canonical.is_dir() {
        anyhow::bail!("Spawn target must be a directory: {}", canonical.display());
    }

    Ok(canonical)
}

fn resolve_for_inspection(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("Spawn target inspection requires an absolute path");
    }

    let normalized = normalize_path(path);
    if normalized.exists() {
        Ok(normalized.canonicalize().unwrap_or(normalized))
    } else {
        Ok(normalized)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn normalize_name(name: Option<&str>) -> Option<String> {
    name.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn default_target_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path_string(path))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn git_stdout<I, S>(path: &Path, args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_succeeds<I, S>(path: &Path, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn repo_name_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches(".git");

    if let Some((prefix, suffix)) = trimmed.rsplit_once(':') {
        if prefix.contains('@') && suffix.contains('/') {
            return non_empty(suffix.to_string());
        }
    }

    let mut segments = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .rev();
    let repo = segments.next()?;
    let owner = segments.next()?;
    non_empty(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn registry_for(dir: &TempDir) -> SpawnTargetRegistry {
        SpawnTargetRegistry::new(dir.path().join("spawn_targets.json"))
    }

    fn run_git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} should succeed", args);
    }

    #[test]
    fn add_canonicalizes_and_lists_targets() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, None, None).unwrap();
        let listed = registry.list().unwrap();

        assert_eq!(listed, vec![added.clone()]);
        assert_eq!(
            added.path,
            target_dir.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(added.name, "project");
        assert!(added.enabled);
    }

    #[test]
    fn add_reuses_existing_target_for_same_canonical_path() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let first = registry.add(&target_dir, Some("Project"), None).unwrap();
        let second = registry.add(target_dir.join("."), Some("Renamed"), None).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.name, "Renamed");
        assert_eq!(registry.list().unwrap().len(), 1);
    }

    #[test]
    fn get_returns_target_by_id() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, Some("Project"), None).unwrap();

        assert_eq!(registry.get(&added.id).unwrap(), Some(added));
        assert_eq!(registry.get("missing").unwrap(), None);
    }

    #[test]
    fn disable_enable_and_update_round_trip() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, Some("Project"), None).unwrap();
        let disabled = registry.disable(&added.id).unwrap().unwrap();
        let renamed = registry
            .update(&added.id, Some("Renamed"), Some(true), None)
            .unwrap()
            .unwrap();

        assert!(!disabled.enabled);
        assert!(renamed.enabled);
        assert_eq!(renamed.name, "Renamed");
    }

    #[test]
    fn remove_deletes_target_from_registry() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, Some("Project"), None).unwrap();
        let removed = registry.remove(&added.id).unwrap();

        assert!(removed.is_some());
        assert_eq!(registry.get(&added.id).unwrap(), None);
        assert!(registry.list().unwrap().is_empty());
    }

    #[test]
    fn inspect_reports_plain_directory_and_admission() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("plain");
        fs::create_dir_all(target_dir.join(".botster")).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, None, None).unwrap();
        let inspection = registry.inspect(&target_dir).unwrap();

        assert!(inspection.exists);
        assert!(inspection.is_directory);
        assert!(inspection.admitted);
        assert_eq!(
            inspection.admitted_target_id.as_deref(),
            Some(added.id.as_str())
        );
        assert!(inspection.admitted_target_enabled);
        assert!(!inspection.is_git_repo);
        assert!(inspection.has_botster_dir);
        assert!(!inspection.supports_worktrees);
    }

    #[test]
    fn inspect_reports_git_capabilities_fresh() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();

        run_git(&repo_dir, &["init", "-b", "main"]);
        run_git(
            &repo_dir,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:wiedymi/restty.git",
            ],
        );

        let registry = registry_for(&temp);
        let inspection = registry.inspect(&repo_dir).unwrap();

        assert!(inspection.is_git_repo);
        assert_eq!(inspection.current_branch.as_deref(), Some("main"));
        assert_eq!(inspection.repo_name.as_deref(), Some("wiedymi/restty"));
        assert_eq!(
            inspection.repo_root.as_deref(),
            Some(repo_dir.canonicalize().unwrap().to_string_lossy().as_ref())
        );
        assert!(inspection.supports_worktrees);
    }

    #[test]
    fn inspect_normalizes_nonexistent_paths_without_failing() {
        let temp = TempDir::new().unwrap();
        let registry = registry_for(&temp);
        let missing = temp.path().join("missing").join("..").join("candidate");

        let inspection = registry.inspect(&missing).unwrap();

        assert!(!inspection.exists);
        assert!(!inspection.is_directory);
        assert!(!inspection.admitted);
        assert!(!inspection.is_git_repo);
        assert!(inspection.path.ends_with("/candidate"));
    }

    #[test]
    fn inspect_rejects_relative_paths() {
        let temp = TempDir::new().unwrap();
        let registry = registry_for(&temp);

        let err = registry.inspect(Path::new("relative/path")).unwrap_err();

        assert!(err
            .to_string()
            .contains("Spawn target inspection requires an absolute path"));
    }

    #[test]
    fn add_with_plugins_persists_and_returns_them() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let plugins = vec!["github".to_string(), "orchestrator".to_string()];
        let added = registry
            .add(&target_dir, Some("Project"), Some(plugins.clone()))
            .unwrap();

        assert_eq!(added.plugins, Some(plugins.clone()));

        let fetched = registry.get(&added.id).unwrap().unwrap();
        assert_eq!(fetched.plugins, Some(plugins.clone()));

        let listed = registry.list().unwrap();
        assert_eq!(listed[0].plugins, Some(plugins));
    }

    #[test]
    fn add_without_plugins_defaults_to_none() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, None, None).unwrap();

        assert_eq!(added.plugins, None);
    }

    #[test]
    fn update_plugins_changes_them() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, Some("Project"), None).unwrap();
        assert_eq!(added.plugins, None);

        let plugins = vec!["github".to_string()];
        let updated = registry
            .update(&added.id, None, None, Some(plugins.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(updated.plugins, Some(plugins));

        let new_plugins = vec!["orchestrator".to_string(), "messaging".to_string()];
        let updated2 = registry
            .update(&added.id, None, None, Some(new_plugins.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(updated2.plugins, Some(new_plugins));
    }

    #[test]
    fn update_none_plugins_does_not_clear_existing() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let plugins = vec!["github".to_string()];
        let added = registry
            .add(&target_dir, Some("Project"), Some(plugins.clone()))
            .unwrap();

        let updated = registry
            .update(&added.id, Some("Renamed"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(updated.plugins, Some(plugins));
        assert_eq!(updated.name, "Renamed");
    }

    #[test]
    fn update_empty_plugins_clears_to_empty_list() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry
            .add(&target_dir, Some("Project"), Some(vec!["github".to_string()]))
            .unwrap();

        let updated = registry
            .update(&added.id, None, None, Some(vec![]))
            .unwrap()
            .unwrap();
        assert_eq!(updated.plugins, Some(vec![]));
    }

    #[test]
    fn plugins_json_round_trip() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let plugins = vec!["github".to_string(), "orchestrator".to_string()];
        let added = registry
            .add(&target_dir, Some("Project"), Some(plugins.clone()))
            .unwrap();

        // Create a fresh registry pointing at the same file to force re-read
        let registry2 = registry_for(&temp);
        let reloaded = registry2.get(&added.id).unwrap().unwrap();
        assert_eq!(reloaded.plugins, Some(plugins));
    }

    #[test]
    fn readd_with_plugins_updates_existing() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();

        let registry = registry_for(&temp);
        let added = registry.add(&target_dir, Some("Project"), None).unwrap();
        assert_eq!(added.plugins, None);

        let plugins = vec!["github".to_string()];
        let readded = registry
            .add(&target_dir, None, Some(plugins.clone()))
            .unwrap();
        assert_eq!(readded.id, added.id);
        assert_eq!(readded.plugins, Some(plugins));
    }
}
