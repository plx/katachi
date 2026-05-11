//! Parse the `[harnesses.gemini]` passthrough config block.
//!
//! `KatachiConfig` stores the Gemini block as an untyped TOML value; this
//! module projects it into a typed [`GeminiConfig`] that the scanner and
//! planner can use directly.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use katachi_core::config::{HarnessConfig, KatachiConfig};
use katachi_core::paths::{expand_tilde, PathError};

pub const HARNESS_NAME: &str = "gemini";

/// Typed view of `[harnesses.gemini]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiConfig {
    pub enabled: bool,
    pub binary: String,
    pub default_backend: String,
    pub home: Option<Utf8PathBuf>,
    #[serde(default)]
    pub extension_roots: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub user_roots: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub project_roots: Vec<Utf8PathBuf>,
    #[serde(default = "default_preserve_overlays")]
    pub preserve_failed_overlays: bool,
    #[serde(default = "default_preview_opt_in")]
    pub treat_preview_features_as_opt_in: bool,
}

fn default_preserve_overlays() -> bool {
    true
}
fn default_preview_opt_in() -> bool {
    true
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: "gemini".into(),
            default_backend: "cli".into(),
            home: None,
            extension_roots: Vec::new(),
            user_roots: Vec::new(),
            project_roots: Vec::new(),
            preserve_failed_overlays: true,
            treat_preview_features_as_opt_in: true,
        }
    }
}

impl GeminiConfig {
    /// Extract a typed config from the `[harnesses.gemini]` section.
    /// Missing sections yield defaults. Tilde-prefixed paths are expanded.
    pub fn from_katachi(config: &KatachiConfig) -> Result<Self, ConfigError> {
        let section = config.harnesses.get(HARNESS_NAME);
        Self::from_harness_config(section)
    }

    pub fn from_harness_config(section: Option<&HarnessConfig>) -> Result<Self, ConfigError> {
        let mut out = Self::default();
        let Some(section) = section else {
            return Ok(out);
        };
        out.enabled = section.enabled;
        if let Some(bin) = &section.binary {
            out.binary = bin.clone();
        }
        if let Some(backend) = &section.default_backend {
            out.default_backend = backend.clone();
        }
        for (key, value) in &section.extra {
            match key.as_str() {
                "home" => {
                    out.home = Some(parse_path(key, value)?);
                }
                "extension_roots" => {
                    out.extension_roots = parse_paths(key, value)?;
                }
                "user_roots" => {
                    out.user_roots = parse_paths(key, value)?;
                }
                "project_roots" => {
                    out.project_roots = parse_paths(key, value)?;
                }
                "preserve_failed_overlays" => {
                    out.preserve_failed_overlays = parse_bool(key, value)?;
                }
                "treat_preview_features_as_opt_in" => {
                    out.treat_preview_features_as_opt_in = parse_bool(key, value)?;
                }
                _ => {
                    // Unknown keys are preserved silently — future-compat.
                }
            }
        }
        Ok(out)
    }

    /// Resolve extension roots against a fallback home directory. If the
    /// config didn't provide any, defaults to `<home>/extensions`.
    pub fn resolved_extension_roots(&self, home: &Utf8Path) -> Vec<Utf8PathBuf> {
        if self.extension_roots.is_empty() {
            vec![home.join("extensions")]
        } else {
            self.extension_roots.clone()
        }
    }

    /// Resolve the configured home directory, or fall back to `~/.gemini`.
    pub fn resolved_home(&self) -> Result<Utf8PathBuf, PathError> {
        match &self.home {
            Some(p) => Ok(p.clone()),
            None => expand_tilde(Utf8Path::new("~/.gemini")),
        }
    }

    pub fn resolved_user_roots(&self, home: &Utf8Path) -> Vec<Utf8PathBuf> {
        if self.user_roots.is_empty() {
            vec![home.to_path_buf()]
        } else {
            self.user_roots.clone()
        }
    }

    pub fn resolved_project_roots(&self, cwd: &Utf8Path) -> Vec<Utf8PathBuf> {
        if self.project_roots.is_empty() {
            vec![cwd.to_path_buf()]
        } else {
            self.project_roots
                .iter()
                .map(|p| {
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        cwd.join(p)
                    }
                })
                .collect()
        }
    }
}

fn parse_path(key: &str, value: &toml::Value) -> Result<Utf8PathBuf, ConfigError> {
    let s = value.as_str().ok_or_else(|| ConfigError::ExpectedString {
        key: key.to_owned(),
    })?;
    expand_tilde(Utf8Path::new(s)).map_err(ConfigError::Path)
}

fn parse_paths(key: &str, value: &toml::Value) -> Result<Vec<Utf8PathBuf>, ConfigError> {
    let arr = value.as_array().ok_or_else(|| ConfigError::ExpectedArray {
        key: key.to_owned(),
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v.as_str().ok_or_else(|| ConfigError::ExpectedString {
            key: format!("{key}[{i}]"),
        })?;
        out.push(expand_tilde(Utf8Path::new(s)).map_err(ConfigError::Path)?);
    }
    Ok(out)
}

fn parse_bool(key: &str, value: &toml::Value) -> Result<bool, ConfigError> {
    value.as_bool().ok_or_else(|| ConfigError::ExpectedBool {
        key: key.to_owned(),
    })
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("gemini config key `{key}` must be a string")]
    ExpectedString { key: String },
    #[error("gemini config key `{key}` must be an array of strings")]
    ExpectedArray { key: String },
    #[error("gemini config key `{key}` must be a boolean")]
    ExpectedBool { key: String },
    #[error(transparent)]
    Path(#[from] PathError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_from_toml(body: &str) -> GeminiConfig {
        let cfg: KatachiConfig = toml::from_str(body).unwrap();
        GeminiConfig::from_katachi(&cfg).unwrap()
    }

    #[test]
    fn empty_config_yields_defaults() {
        let cfg = KatachiConfig::default();
        let g = GeminiConfig::from_katachi(&cfg).unwrap();
        assert!(g.enabled);
        assert_eq!(g.binary, "gemini");
        assert_eq!(g.default_backend, "cli");
        assert!(g.extension_roots.is_empty());
        assert!(g.preserve_failed_overlays);
        assert!(g.treat_preview_features_as_opt_in);
    }

    #[test]
    fn parses_typed_fields_and_arrays() {
        let body = r#"
version = 1

[harnesses.gemini]
enabled = true
binary = "gemini-preview"
default_backend = "cli"
home = "/custom/home/.gemini"
extension_roots = ["/custom/home/.gemini/extensions", "/shared/ext"]
user_roots = ["/custom/home/.gemini"]
project_roots = ["."]
preserve_failed_overlays = false
treat_preview_features_as_opt_in = false
"#;
        let g = parse_from_toml(body);
        assert_eq!(g.binary, "gemini-preview");
        assert_eq!(g.home.as_deref().unwrap().as_str(), "/custom/home/.gemini");
        assert_eq!(g.extension_roots.len(), 2);
        assert_eq!(g.user_roots.len(), 1);
        assert_eq!(g.project_roots.len(), 1);
        assert!(!g.preserve_failed_overlays);
        assert!(!g.treat_preview_features_as_opt_in);
    }

    #[test]
    fn extension_roots_default_to_home_subdir() {
        let g = GeminiConfig::default();
        let roots = g.resolved_extension_roots(Utf8Path::new("/home/.gemini"));
        assert_eq!(roots, vec![Utf8PathBuf::from("/home/.gemini/extensions")]);
    }

    #[test]
    fn relative_project_roots_get_joined_to_cwd() {
        let g = GeminiConfig {
            project_roots: vec![Utf8PathBuf::from("subdir")],
            ..GeminiConfig::default()
        };
        let roots = g.resolved_project_roots(Utf8Path::new("/workspace"));
        assert_eq!(roots, vec![Utf8PathBuf::from("/workspace/subdir")]);
    }

    #[test]
    fn type_mismatch_in_extension_roots_is_error() {
        let body = r#"
version = 1

[harnesses.gemini]
extension_roots = 3
"#;
        let cfg: KatachiConfig = toml::from_str(body).unwrap();
        let err = GeminiConfig::from_katachi(&cfg).unwrap_err();
        assert!(matches!(err, ConfigError::ExpectedArray { .. }));
    }

    #[test]
    fn unknown_keys_preserved_silently() {
        let body = r#"
version = 1

[harnesses.gemini]
future_field = "still-ok"
"#;
        let cfg: KatachiConfig = toml::from_str(body).unwrap();
        let _g = GeminiConfig::from_katachi(&cfg).unwrap();
    }
}
