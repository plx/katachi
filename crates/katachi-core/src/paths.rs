//! Storage path discovery for katachi.
//!
//! katachi stores three logical locations on disk:
//!
//! - the **config file** (`config.toml`)
//! - the **data root** (runs, katachis, rosters)
//! - the **cache root** (discovery caches, parsed graphs)
//!
//! Each location is resolved independently, layered from most specific to
//! most general:
//!
//! 1. explicit CLI override (e.g. `--config <path>`)
//! 2. environment variable (`KATACHI_CONFIG`, `KATACHI_DATA`, `KATACHI_CACHE`)
//! 3. config file `[storage]` override (data/cache only)
//! 4. XDG base-directory default (`$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_CACHE_HOME`)
//! 5. legacy home fallback (`~/.katachi/` and its `cache/` subdirectory)
//!
//! The legacy fallback is consulted only when the XDG default does **not**
//! already exist and the legacy path **does**. This preserves existing
//! installations without masking XDG paths on fresh setups.
//!
//! All paths are UTF-8 (`Utf8PathBuf`). Tilde expansion is performed for
//! overrides coming from CLI flags, env vars, and the config file.
//!
//! Non-existent paths are allowed; the caller decides whether to create them.

use std::env;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while resolving storage paths.
#[derive(Debug, Error)]
pub enum PathError {
    #[error("unable to determine the user's home directory")]
    NoHomeDir,
    #[error("path `{0}` is not valid UTF-8")]
    NonUtf8Path(String),
}

/// Records where a resolved path came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathSource {
    /// Explicit CLI flag (e.g. `--config`).
    CliFlag,
    /// Environment variable override.
    EnvVar { name: String },
    /// Override from the loaded config file `[storage]` section.
    ConfigFile,
    /// XDG base-directory default.
    Xdg,
    /// Legacy `~/.katachi/` layout, used when it exists and XDG does not.
    LegacyHome,
    /// XDG default chosen because no concrete location exists yet.
    XdgDefault,
}

/// A resolved path paired with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPath {
    pub path: Utf8PathBuf,
    pub source: PathSource,
}

/// Inputs callers hand to path resolution.
///
/// Each field is independent. Populate from CLI flags (highest priority)
/// or leave `None` to fall back to env vars / XDG / legacy discovery.
#[derive(Debug, Clone, Default)]
pub struct PathOverrides {
    pub config_file: Option<Utf8PathBuf>,
    pub data_root: Option<Utf8PathBuf>,
    pub cache_root: Option<Utf8PathBuf>,
}

/// Config-file-sourced storage overrides. Mirror of the `[storage]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Data root for runs, katachis, and rosters.
    pub data: Option<Utf8PathBuf>,
    /// Cache root for discovery caches.
    pub cache: Option<Utf8PathBuf>,
}

/// Fully resolved data/cache locations plus well-known subpaths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePaths {
    pub data_root: ResolvedPath,
    pub cache_root: ResolvedPath,
}

impl StoragePaths {
    pub fn runs_dir(&self) -> Utf8PathBuf {
        self.data_root.path.join("runs")
    }
    pub fn katachis_dir(&self) -> Utf8PathBuf {
        self.data_root.path.join("katachis")
    }
    pub fn rosters_dir(&self) -> Utf8PathBuf {
        self.data_root.path.join("rosters")
    }
}

/// Resolve the configuration file path.
///
/// The configuration file may or may not exist on disk. Callers are
/// expected to tolerate a missing file and fall back to defaults.
pub fn resolve_config_file(overrides: &PathOverrides) -> Result<ResolvedPath, PathError> {
    if let Some(explicit) = &overrides.config_file {
        return Ok(ResolvedPath {
            path: expand_tilde(explicit)?,
            source: PathSource::CliFlag,
        });
    }
    if let Some(from_env) = env_override("KATACHI_CONFIG")? {
        return Ok(ResolvedPath {
            path: from_env,
            source: PathSource::EnvVar { name: "KATACHI_CONFIG".into() },
        });
    }

    let xdg = xdg_config_dir()?.join("katachi").join("config.toml");
    if xdg.exists() {
        return Ok(ResolvedPath { path: xdg, source: PathSource::Xdg });
    }

    let legacy = legacy_home()?.join("config.toml");
    if legacy.exists() {
        return Ok(ResolvedPath { path: legacy, source: PathSource::LegacyHome });
    }

    // Nothing exists. Default to the XDG path; caller will decide whether
    // to create it or proceed with pure defaults.
    Ok(ResolvedPath { path: xdg, source: PathSource::XdgDefault })
}

/// Resolve the data and cache roots after the config file has been loaded.
pub fn resolve_storage_paths(
    overrides: &PathOverrides,
    storage_config: &StorageConfig,
) -> Result<StoragePaths, PathError> {
    let data_root = resolve_data_root(overrides, storage_config)?;
    let cache_root = resolve_cache_root(overrides, storage_config)?;
    Ok(StoragePaths { data_root, cache_root })
}

fn resolve_data_root(
    overrides: &PathOverrides,
    storage_config: &StorageConfig,
) -> Result<ResolvedPath, PathError> {
    if let Some(cli) = &overrides.data_root {
        return Ok(ResolvedPath {
            path: expand_tilde(cli)?,
            source: PathSource::CliFlag,
        });
    }
    if let Some(from_env) = env_override("KATACHI_DATA")? {
        return Ok(ResolvedPath {
            path: from_env,
            source: PathSource::EnvVar { name: "KATACHI_DATA".into() },
        });
    }
    if let Some(from_config) = &storage_config.data {
        return Ok(ResolvedPath {
            path: expand_tilde(from_config)?,
            source: PathSource::ConfigFile,
        });
    }

    let xdg = xdg_data_dir()?.join("katachi");
    if xdg.exists() {
        return Ok(ResolvedPath { path: xdg, source: PathSource::Xdg });
    }

    let legacy = legacy_home()?;
    if legacy.exists() {
        return Ok(ResolvedPath { path: legacy, source: PathSource::LegacyHome });
    }

    Ok(ResolvedPath { path: xdg, source: PathSource::XdgDefault })
}

fn resolve_cache_root(
    overrides: &PathOverrides,
    storage_config: &StorageConfig,
) -> Result<ResolvedPath, PathError> {
    if let Some(cli) = &overrides.cache_root {
        return Ok(ResolvedPath {
            path: expand_tilde(cli)?,
            source: PathSource::CliFlag,
        });
    }
    if let Some(from_env) = env_override("KATACHI_CACHE")? {
        return Ok(ResolvedPath {
            path: from_env,
            source: PathSource::EnvVar { name: "KATACHI_CACHE".into() },
        });
    }
    if let Some(from_config) = &storage_config.cache {
        return Ok(ResolvedPath {
            path: expand_tilde(from_config)?,
            source: PathSource::ConfigFile,
        });
    }

    let xdg = xdg_cache_dir()?.join("katachi");
    if xdg.exists() {
        return Ok(ResolvedPath { path: xdg, source: PathSource::Xdg });
    }

    let legacy = legacy_home()?.join("cache");
    if legacy.exists() {
        return Ok(ResolvedPath { path: legacy, source: PathSource::LegacyHome });
    }

    Ok(ResolvedPath { path: xdg, source: PathSource::XdgDefault })
}

fn env_override(name: &str) -> Result<Option<Utf8PathBuf>, PathError> {
    match env::var(name) {
        Ok(raw) if !raw.is_empty() => {
            let expanded = expand_tilde(Utf8Path::new(&raw))?;
            Ok(Some(expanded))
        }
        _ => Ok(None),
    }
}

fn xdg_config_dir() -> Result<Utf8PathBuf, PathError> {
    xdg_dir_or_fallback("XDG_CONFIG_HOME", ".config")
}

fn xdg_data_dir() -> Result<Utf8PathBuf, PathError> {
    xdg_dir_or_fallback("XDG_DATA_HOME", ".local/share")
}

fn xdg_cache_dir() -> Result<Utf8PathBuf, PathError> {
    xdg_dir_or_fallback("XDG_CACHE_HOME", ".cache")
}

fn xdg_dir_or_fallback(env_name: &str, home_suffix: &str) -> Result<Utf8PathBuf, PathError> {
    if let Ok(raw) = env::var(env_name) {
        if !raw.is_empty() {
            return expand_tilde(Utf8Path::new(&raw));
        }
    }
    Ok(home_dir()?.join(home_suffix))
}

fn legacy_home() -> Result<Utf8PathBuf, PathError> {
    Ok(home_dir()?.join(".katachi"))
}

fn home_dir() -> Result<Utf8PathBuf, PathError> {
    let home = dirs::home_dir().ok_or(PathError::NoHomeDir)?;
    Utf8PathBuf::from_path_buf(home).map_err(|p| PathError::NonUtf8Path(p.to_string_lossy().into()))
}

/// Expand a leading `~` or `~/` to the user's home directory.
pub fn expand_tilde(path: &Utf8Path) -> Result<Utf8PathBuf, PathError> {
    let s = path.as_str();
    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    // Env vars are process-global. Serialize tests that mutate them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        keys: Vec<(&'static str, Option<String>)>,
    }
    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys.iter().map(|k| (*k, env::var(k).ok())).collect();
            for k in keys {
                env::remove_var(k);
            }
            Self { _lock: lock, keys: saved }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.keys {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
    }

    fn fake_home(g: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(g.path().to_path_buf()).unwrap()
    }

    #[test]
    fn cli_override_beats_env_and_defaults() {
        let _g = EnvGuard::new(&["KATACHI_CONFIG"]);
        env::set_var("KATACHI_CONFIG", "/tmp/ignored.toml");
        let overrides = PathOverrides {
            config_file: Some(Utf8PathBuf::from("/tmp/explicit.toml")),
            ..Default::default()
        };
        let resolved = resolve_config_file(&overrides).unwrap();
        assert_eq!(resolved.path.as_str(), "/tmp/explicit.toml");
        assert_eq!(resolved.source, PathSource::CliFlag);
    }

    #[test]
    fn env_var_override_applies_when_no_cli_flag() {
        let _g = EnvGuard::new(&["KATACHI_CONFIG"]);
        env::set_var("KATACHI_CONFIG", "/tmp/from-env.toml");
        let resolved = resolve_config_file(&PathOverrides::default()).unwrap();
        assert_eq!(resolved.path.as_str(), "/tmp/from-env.toml");
        assert_eq!(
            resolved.source,
            PathSource::EnvVar { name: "KATACHI_CONFIG".into() }
        );
    }

    #[test]
    fn config_file_override_reaches_data_root() {
        let _g = EnvGuard::new(&["KATACHI_DATA"]);
        let overrides = PathOverrides::default();
        let cfg = StorageConfig {
            data: Some(Utf8PathBuf::from("/tmp/katachi-data")),
            ..Default::default()
        };
        let resolved = resolve_storage_paths(&overrides, &cfg).unwrap();
        assert_eq!(resolved.data_root.path.as_str(), "/tmp/katachi-data");
        assert_eq!(resolved.data_root.source, PathSource::ConfigFile);
    }

    #[test]
    fn xdg_default_used_when_nothing_exists() {
        let _g = EnvGuard::new(&[
            "KATACHI_CONFIG",
            "KATACHI_DATA",
            "KATACHI_CACHE",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "HOME",
        ]);
        let home = TempDir::new().unwrap();
        env::set_var("HOME", home.path());
        let xdg_config = fake_home(&home).join(".config");
        let resolved = resolve_config_file(&PathOverrides::default()).unwrap();
        assert_eq!(
            resolved.path,
            xdg_config.join("katachi").join("config.toml")
        );
        assert_eq!(resolved.source, PathSource::XdgDefault);
    }

    #[test]
    fn legacy_home_picked_up_when_present_and_xdg_absent() {
        let _g = EnvGuard::new(&[
            "KATACHI_CONFIG",
            "XDG_CONFIG_HOME",
            "HOME",
        ]);
        let home = TempDir::new().unwrap();
        env::set_var("HOME", home.path());
        let legacy = fake_home(&home).join(".katachi");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.toml"), "").unwrap();

        let resolved = resolve_config_file(&PathOverrides::default()).unwrap();
        assert_eq!(resolved.path, legacy.join("config.toml"));
        assert_eq!(resolved.source, PathSource::LegacyHome);
    }

    #[test]
    fn tilde_expansion() {
        let _g = EnvGuard::new(&["HOME"]);
        env::set_var("HOME", "/tmp/custom-home");
        let expanded = expand_tilde(Utf8Path::new("~/foo/bar")).unwrap();
        assert_eq!(expanded.as_str(), "/tmp/custom-home/foo/bar");
    }
}
