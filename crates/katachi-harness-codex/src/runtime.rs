//! Runtime settings parsed from the `[harnesses.codex]` section of the
//! top-level `config.toml`.
//!
//! The katachi shared config keeps harness-specific fields in a passthrough
//! `extra` map; this module decodes the Codex-specific fields into a typed
//! struct so the rest of the crate can consult them without repeatedly
//! fishing keys out of the raw TOML.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::config::{HarnessConfig, KatachiConfig};
use katachi_core::paths::expand_tilde;
use serde::{Deserialize, Serialize};

/// Settings Codex cares about, with sensible defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexSettings {
    /// Path to the Codex binary (defaults to `"codex"`).
    pub binary: String,
    /// Default backend preference (`cli`, `sdk-ts`, `sdk-py`).
    pub default_backend: String,
    /// User-facing `CODEX_HOME` override. `~` expansion applied.
    pub codex_home: Utf8PathBuf,
    /// Project roots the scanner walks for `.codex/config.toml` and
    /// `AGENTS.md` files.
    pub project_roots: Vec<Utf8PathBuf>,
    /// Plugin marketplace roots to scan.
    pub marketplace_roots: Vec<Utf8PathBuf>,
    /// Whether to honor project trust (a `.codex/config.toml` is only
    /// active when the project is trusted).
    pub respect_project_trust: bool,
    /// Whether to keep failed materialized overlays for post-mortem.
    pub preserve_failed_overlays: bool,
    /// Feature flag for the experimental Python SDK path.
    pub enable_python_sdk: bool,
}

impl Default for CodexSettings {
    fn default() -> Self {
        let home = expand_tilde(Utf8Path::new("~/.codex")).unwrap_or_else(|_| "~/.codex".into());
        let default_marketplace = [
            expand_tilde(Utf8Path::new("~/.agents/plugins"))
                .unwrap_or_else(|_| "~/.agents/plugins".into()),
            Utf8PathBuf::from(".agents/plugins"),
        ];
        Self {
            binary: "codex".into(),
            default_backend: "cli".into(),
            codex_home: home,
            project_roots: vec![Utf8PathBuf::from(".")],
            marketplace_roots: default_marketplace.to_vec(),
            respect_project_trust: true,
            preserve_failed_overlays: true,
            enable_python_sdk: false,
        }
    }
}

impl CodexSettings {
    /// Load Codex settings from a [`KatachiConfig`]. Missing or disabled
    /// harness entries collapse to defaults.
    pub fn load(config: &KatachiConfig) -> Self {
        let base = config.harnesses.get("codex");
        Self::from_harness_config(base)
    }

    /// Build settings from a specific [`HarnessConfig`].
    pub fn from_harness_config(hc: Option<&HarnessConfig>) -> Self {
        let mut settings = Self::default();
        let Some(hc) = hc else { return settings };

        if let Some(bin) = &hc.binary {
            settings.binary = bin.clone();
        }
        if let Some(backend) = &hc.default_backend {
            settings.default_backend = backend.clone();
        }

        if let Some(v) = hc.extra.get("codex_home") {
            if let Some(s) = v.as_str() {
                settings.codex_home =
                    expand_tilde(Utf8Path::new(s)).unwrap_or_else(|_| s.into());
            }
        }
        if let Some(v) = hc.extra.get("project_roots") {
            settings.project_roots = decode_path_list(v);
        }
        if let Some(v) = hc.extra.get("marketplace_roots") {
            settings.marketplace_roots = decode_path_list(v);
        }
        if let Some(v) = hc.extra.get("respect_project_trust") {
            if let Some(b) = v.as_bool() {
                settings.respect_project_trust = b;
            }
        }
        if let Some(v) = hc.extra.get("preserve_failed_overlays") {
            if let Some(b) = v.as_bool() {
                settings.preserve_failed_overlays = b;
            }
        }
        if let Some(v) = hc.extra.get("enable_python_sdk") {
            if let Some(b) = v.as_bool() {
                settings.enable_python_sdk = b;
            }
        }

        settings
    }
}

fn decode_path_list(v: &toml::Value) -> Vec<Utf8PathBuf> {
    match v {
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| expand_tilde(Utf8Path::new(s)).unwrap_or_else(|_| s.into()))
            .collect(),
        toml::Value::String(s) => vec![expand_tilde(Utf8Path::new(s)).unwrap_or_else(|_| s.into())],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn defaults_are_sane() {
        let s = CodexSettings::default();
        assert_eq!(s.binary, "codex");
        assert_eq!(s.default_backend, "cli");
        assert!(s.respect_project_trust);
        assert!(s.preserve_failed_overlays);
        assert!(!s.enable_python_sdk);
        assert_eq!(s.project_roots, vec![Utf8PathBuf::from(".")]);
    }

    #[test]
    fn load_reads_top_level_harness_block() {
        let toml = r#"
version = 1

[harnesses.codex]
enabled = true
binary = "/opt/codex/bin/codex"
default_backend = "cli"
codex_home = "/tmp/alt-codex"
project_roots = ["./sub"]
marketplace_roots = ["/tmp/market"]
respect_project_trust = false
preserve_failed_overlays = false
enable_python_sdk = true
"#;
        let cfg: KatachiConfig = toml::from_str(toml).unwrap();
        let s = CodexSettings::load(&cfg);
        assert_eq!(s.binary, "/opt/codex/bin/codex");
        assert_eq!(s.codex_home, Utf8PathBuf::from("/tmp/alt-codex"));
        assert_eq!(s.project_roots, vec![Utf8PathBuf::from("./sub")]);
        assert_eq!(s.marketplace_roots, vec![Utf8PathBuf::from("/tmp/market")]);
        assert!(!s.respect_project_trust);
        assert!(!s.preserve_failed_overlays);
        assert!(s.enable_python_sdk);
    }

    #[test]
    fn missing_harness_block_yields_defaults() {
        let cfg = KatachiConfig::default();
        let s = CodexSettings::load(&cfg);
        assert_eq!(s, CodexSettings::default());
    }

    #[test]
    fn unknown_fields_ignored() {
        let mut extra: IndexMap<String, toml::Value> = IndexMap::new();
        extra.insert("some-new-thing".into(), toml::Value::Boolean(true));
        extra.insert("codex_home".into(), toml::Value::String("/x".into()));
        let hc = HarnessConfig {
            enabled: true,
            binary: None,
            default_backend: None,
            extra,
        };
        let s = CodexSettings::from_harness_config(Some(&hc));
        assert_eq!(s.codex_home, Utf8PathBuf::from("/x"));
    }
}
