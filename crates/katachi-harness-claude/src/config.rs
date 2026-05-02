//! Claude-specific configuration projection.
//!
//! The shared [`katachi_core::config::KatachiConfig`] keeps per-harness
//! settings in a passthrough `IndexMap<String, toml::Value>` so harness
//! crates can read their own schema without forcing changes upstream.
//! This module pulls the Claude-specific slice into a typed struct.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::config::KatachiConfig;
use serde::{Deserialize, Serialize};

const DEFAULT_BINARY: &str = "claude";

/// Claude-specific slice of the shared config.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeConfig {
    /// Whether the Claude harness is enabled at all.
    pub enabled: bool,
    /// Binary name (or absolute path) for `claude`.
    pub binary: String,
    /// Preferred backend (`cli`, `sdk-ts`, `sdk-py`).
    pub default_backend: String,
    /// Roots to scan for installed Claude plugins.
    pub plugin_roots: Vec<Utf8PathBuf>,
    /// User-scoped `.claude/` root.
    pub user_root: Utf8PathBuf,
    /// Project-scoped roots (usually `["."]`).
    pub project_roots: Vec<Utf8PathBuf>,
    /// Setting sources to enable by default.
    pub default_setting_sources: Vec<String>,
    /// Whether the planner prefers materialized overlays over ambient CLI.
    pub prefer_materialized_cli: bool,
    /// Whether failed overlays should be preserved on disk for analysis.
    pub preserve_failed_overlays: bool,
    /// Where Claude rosters live, relative to `<data_root>/rosters/claude/`.
    /// Empty string means use the default.
    pub roster_dir_override: Option<Utf8PathBuf>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            binary: DEFAULT_BINARY.to_string(),
            default_backend: "cli".to_string(),
            plugin_roots: vec![Utf8PathBuf::from("~/.claude/plugins")],
            user_root: Utf8PathBuf::from("~/.claude"),
            project_roots: vec![Utf8PathBuf::from(".")],
            default_setting_sources: vec!["user".into(), "project".into()],
            prefer_materialized_cli: true,
            preserve_failed_overlays: true,
            roster_dir_override: None,
        }
    }
}

impl ClaudeConfig {
    /// Read the `[harnesses.claude]` section out of a shared config.
    ///
    /// Unknown keys are simply ignored — harness config is intentionally
    /// tolerant of extra data that other tooling may inject.
    pub fn from_shared(config: &KatachiConfig) -> Self {
        let mut out = Self::default();
        let Some(h) = config.harnesses.get("claude") else {
            return out;
        };
        out.enabled = h.enabled;
        if let Some(bin) = &h.binary {
            out.binary = bin.clone();
        }
        if let Some(b) = &h.default_backend {
            out.default_backend = b.clone();
        }
        if let Some(v) = h.extra.get("plugin_roots").and_then(|v| v.as_array()) {
            out.plugin_roots = v
                .iter()
                .filter_map(|x| x.as_str())
                .map(Utf8PathBuf::from)
                .collect();
        }
        if let Some(v) = h.extra.get("user_root").and_then(|v| v.as_str()) {
            out.user_root = Utf8PathBuf::from(v);
        }
        if let Some(v) = h.extra.get("project_roots").and_then(|v| v.as_array()) {
            out.project_roots = v
                .iter()
                .filter_map(|x| x.as_str())
                .map(Utf8PathBuf::from)
                .collect();
        }
        if let Some(v) = h
            .extra
            .get("default_setting_sources")
            .and_then(|v| v.as_array())
        {
            out.default_setting_sources =
                v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect();
        }
        if let Some(v) = h.extra.get("prefer_materialized_cli").and_then(|v| v.as_bool()) {
            out.prefer_materialized_cli = v;
        }
        if let Some(v) = h
            .extra
            .get("preserve_failed_overlays")
            .and_then(|v| v.as_bool())
        {
            out.preserve_failed_overlays = v;
        }
        if let Some(v) = h.extra.get("roster_dir").and_then(|v| v.as_str()) {
            out.roster_dir_override = Some(Utf8PathBuf::from(v));
        }
        out
    }

    /// Expand leading `~/` in each configured path against `$HOME`. Paths
    /// that can't be expanded fall through unchanged, which is fine for
    /// discovery — those paths just won't exist.
    pub fn expand_home(&mut self) {
        if let Some(home) = dirs::home_dir().and_then(|p| Utf8PathBuf::from_path_buf(p).ok()) {
            self.plugin_roots = self
                .plugin_roots
                .iter()
                .map(|p| expand_tilde(p, &home))
                .collect();
            self.user_root = expand_tilde(&self.user_root, &home);
            self.project_roots = self
                .project_roots
                .iter()
                .map(|p| expand_tilde(p, &home))
                .collect();
            if let Some(dir) = &self.roster_dir_override {
                self.roster_dir_override = Some(expand_tilde(dir, &home));
            }
        }
    }
}

fn expand_tilde(path: &Utf8Path, home: &Utf8Path) -> Utf8PathBuf {
    if path.as_str() == "~" {
        return home.to_owned();
    }
    if let Some(rest) = path.as_str().strip_prefix("~/") {
        return home.join(rest);
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use katachi_core::config::HarnessConfig;

    #[test]
    fn defaults_match_spec() {
        let c = ClaudeConfig::default();
        assert!(c.enabled);
        assert_eq!(c.binary, "claude");
        assert_eq!(c.default_backend, "cli");
        assert_eq!(c.plugin_roots.len(), 1);
        assert!(c.prefer_materialized_cli);
        assert!(c.preserve_failed_overlays);
    }

    #[test]
    fn reads_custom_plugin_roots_and_binary() {
        let mut extra: IndexMap<String, toml::Value> = IndexMap::new();
        extra.insert(
            "plugin_roots".into(),
            toml::Value::Array(vec![
                toml::Value::String("/opt/plugins".into()),
                toml::Value::String("/tmp/ext".into()),
            ]),
        );
        extra.insert(
            "default_setting_sources".into(),
            toml::Value::Array(vec![toml::Value::String("user".into())]),
        );
        let mut config = KatachiConfig::default();
        config.harnesses.insert(
            "claude".into(),
            HarnessConfig {
                enabled: true,
                binary: Some("claude-stable".into()),
                default_backend: Some("sdk-ts".into()),
                extra,
            },
        );
        let c = ClaudeConfig::from_shared(&config);
        assert_eq!(c.binary, "claude-stable");
        assert_eq!(c.default_backend, "sdk-ts");
        assert_eq!(c.plugin_roots.len(), 2);
        assert_eq!(c.plugin_roots[0], Utf8PathBuf::from("/opt/plugins"));
        assert_eq!(c.default_setting_sources, vec!["user".to_string()]);
    }

    #[test]
    fn tilde_expansion_normalizes_paths() {
        let mut c = ClaudeConfig::default();
        c.expand_home();
        assert!(
            !c.user_root.as_str().starts_with('~'),
            "tilde should be expanded, got `{}`",
            c.user_root
        );
    }

    #[test]
    fn tilde_expansion_normalizes_project_roots() {
        let mut c = ClaudeConfig::default();
        c.project_roots = vec![Utf8PathBuf::from("~/repo")];
        c.expand_home();
        if dirs::home_dir().is_some() {
            assert!(
                !c.project_roots[0].as_str().starts_with('~'),
                "project_roots tilde must be expanded, got `{}`",
                c.project_roots[0]
            );
        }
    }

    #[test]
    fn tilde_expansion_leaves_relative_paths_unchanged() {
        let mut c = ClaudeConfig::default();
        c.project_roots = vec![Utf8PathBuf::from(".")];
        c.expand_home();
        assert_eq!(c.project_roots[0], Utf8PathBuf::from("."));
    }
}
