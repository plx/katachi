//! Config loading for katachi.
//!
//! User-facing config lives in `config.toml` at the location resolved by
//! [`crate::paths::resolve_config_file`]. This module parses it into a typed
//! [`KatachiConfig`] and, when no file exists, returns sensible defaults.
//!
//! Harness-specific configuration is kept partly typed (the fields katachi
//! actually needs) and partly as a `toml::Value` passthrough (`extra`), so
//! harness modules can read their own settings without requiring changes here.

use camino::Utf8Path;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::{PathError, ResolvedPath, StorageConfig};

/// Top-level config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KatachiConfig {
    /// Schema version. Currently always `1`.
    #[serde(default = "default_version")]
    pub version: u32,

    #[serde(default)]
    pub defaults: DefaultsConfig,

    #[serde(default)]
    pub storage: StorageConfig,

    /// One entry per harness. Key is the harness short name (`claude`,
    /// `codex`, `gemini`). Unknown keys are preserved as well so that the
    /// config can evolve without requiring shared-layer changes.
    #[serde(default)]
    pub harnesses: IndexMap<String, HarnessConfig>,
}

impl Default for KatachiConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            defaults: DefaultsConfig::default(),
            storage: StorageConfig::default(),
            harnesses: IndexMap::new(),
        }
    }
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Order in which to try harnesses when resolving ambiguous katachi ids.
    #[serde(default = "default_harness_priority")]
    pub harness_priority: Vec<String>,
    /// Order in which to try backends for a given harness.
    #[serde(default = "default_backend_priority")]
    pub backend_priority: Vec<String>,
    /// Default materialization mode.
    #[serde(default = "default_materialization")]
    pub materialization: String,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            harness_priority: default_harness_priority(),
            backend_priority: default_backend_priority(),
            materialization: default_materialization(),
        }
    }
}

fn default_harness_priority() -> Vec<String> {
    vec!["claude".into(), "codex".into(), "gemini".into()]
}
fn default_backend_priority() -> Vec<String> {
    vec!["cli".into(), "sdk-ts".into(), "sdk-py".into()]
}
fn default_materialization() -> String {
    "temp-overlay".into()
}

/// Per-harness config. Fields katachi actually reads are typed; everything
/// else is preserved in `extra` so harness crates can consult it without
/// extending this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Binary name or path. If unset, the harness short name is used.
    #[serde(default)]
    pub binary: Option<String>,
    /// Preferred backend for this harness (`cli`, `sdk-ts`, `sdk-py`).
    #[serde(default)]
    pub default_backend: Option<String>,
    /// Anything else in the harness section is preserved as raw TOML.
    #[serde(flatten)]
    pub extra: IndexMap<String, toml::Value>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: None,
            default_backend: None,
            extra: IndexMap::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Result of attempting to load config from disk.
#[derive(Debug, Clone)]
pub struct ConfigLoad {
    /// Parsed config. Either loaded from disk or defaulted.
    pub config: KatachiConfig,
    /// Where we looked for the config.
    pub source_path: ResolvedPath,
    /// Whether a file actually existed and was parsed.
    pub loaded_from_disk: bool,
    /// Non-fatal diagnostics (e.g. unknown harness keys preserved via `extra`).
    pub diagnostics: Vec<ConfigDiagnostic>,
}

/// Non-fatal config notes surfaced by `doctor` etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub severity: ConfigSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSeverity {
    Info,
    Warning,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error(transparent)]
    Path(#[from] PathError),
}

/// Load the config at `source`, or return defaults if the file does not exist.
pub fn load(source: ResolvedPath) -> Result<ConfigLoad, ConfigError> {
    let mut diagnostics = Vec::new();
    let path: &Utf8Path = source.path.as_path();

    if !path.exists() {
        diagnostics.push(ConfigDiagnostic {
            severity: ConfigSeverity::Info,
            message: format!(
                "no config file at `{path}`; using defaults. Create it to customize katachi."
            ),
        });
        return Ok(ConfigLoad {
            config: KatachiConfig::default(),
            source_path: source,
            loaded_from_disk: false,
            diagnostics,
        });
    }

    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_string(),
        source,
    })?;
    let config: KatachiConfig =
        toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })?;

    if config.version != 1 {
        diagnostics.push(ConfigDiagnostic {
            severity: ConfigSeverity::Warning,
            message: format!(
                "config `{path}` declares version {} but katachi only understands version 1",
                config.version
            ),
        });
    }

    Ok(ConfigLoad {
        config,
        source_path: source,
        loaded_from_disk: true,
        diagnostics,
    })
}

impl KatachiConfig {
    /// Look up an enabled harness by short name.
    pub fn enabled_harness(&self, name: &str) -> Option<&HarnessConfig> {
        self.harnesses.get(name).filter(|c| c.enabled)
    }

    /// Return the binary name/path for a harness, falling back to the short name.
    pub fn harness_binary(&self, name: &str) -> String {
        self.harnesses
            .get(name)
            .and_then(|c| c.binary.clone())
            .unwrap_or_else(|| name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::PathSource;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn fake_resolved(path: Utf8PathBuf) -> ResolvedPath {
        ResolvedPath { path, source: PathSource::XdgDefault }
    }

    #[test]
    fn missing_file_returns_defaults_with_info() {
        let dir = TempDir::new().unwrap();
        let path =
            Utf8PathBuf::from_path_buf(dir.path().join("does-not-exist.toml")).unwrap();
        let load = load(fake_resolved(path)).unwrap();
        assert!(!load.loaded_from_disk);
        assert_eq!(load.config.version, 1);
        assert_eq!(load.config.defaults.harness_priority, vec!["claude", "codex", "gemini"]);
        assert_eq!(load.diagnostics.len(), 1);
        assert_eq!(load.diagnostics[0].severity, ConfigSeverity::Info);
    }

    #[test]
    fn loads_typed_fields_and_preserves_extras() {
        let dir = TempDir::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("config.toml")).unwrap();
        let body = r#"
version = 1

[defaults]
harness_priority = ["codex", "claude"]
backend_priority = ["cli"]
materialization = "ambient"

[storage]
data = "/custom/data"

[harnesses.claude]
enabled = true
binary = "claude-stable"
default_backend = "cli"
plugin_roots = ["~/.claude/plugins"]

[harnesses.codex]
enabled = false
"#;
        std::fs::write(path.as_path(), body).unwrap();
        let load = load(fake_resolved(path)).unwrap();
        assert!(load.loaded_from_disk);
        assert_eq!(load.config.defaults.harness_priority, vec!["codex", "claude"]);
        assert_eq!(load.config.defaults.materialization, "ambient");

        let claude = load.config.enabled_harness("claude").unwrap();
        assert_eq!(claude.binary.as_deref(), Some("claude-stable"));
        assert_eq!(claude.default_backend.as_deref(), Some("cli"));
        assert!(claude.extra.contains_key("plugin_roots"));

        assert!(load.config.enabled_harness("codex").is_none());
        assert_eq!(load.config.harness_binary("gemini"), "gemini");
        assert_eq!(load.config.storage.data.as_deref().unwrap().as_str(), "/custom/data");
    }

    #[test]
    fn future_version_emits_warning() {
        let dir = TempDir::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("config.toml")).unwrap();
        std::fs::write(path.as_path(), "version = 99\n").unwrap();
        let load = load(fake_resolved(path)).unwrap();
        assert!(load
            .diagnostics
            .iter()
            .any(|d| d.severity == ConfigSeverity::Warning));
    }

    #[test]
    fn parse_error_surfaces_path() {
        let dir = TempDir::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("config.toml")).unwrap();
        std::fs::write(path.as_path(), "version = not-a-number\n").unwrap();
        let err = load(fake_resolved(path.clone())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(path.as_str()), "error should reference path: {msg}");
    }
}
