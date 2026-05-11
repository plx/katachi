//! Claude roster file format.
//!
//! Claude rosters are TOML files that name a plugin/skill/agent bundle
//! plus a run profile. Rosters are harness-scoped: the shared katachi
//! definition points at a roster by id, and the Claude harness resolves
//! that roster into concrete CLI flags and overlay contents.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::paths::StoragePaths;
use serde::{Deserialize, Serialize};

use crate::config::ClaudeConfig;
use crate::error::ClaudeRosterError;

pub const CLAUDE_ROSTER_SCHEMA_VERSION: u32 = 1;

/// Top-level Claude roster file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeRoster {
    #[serde(default = "default_schema")]
    pub version: u32,
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub selection: RosterSelection,
    #[serde(default)]
    pub run_profile: RunProfile,
    #[serde(default)]
    pub resolution: RosterResolution,
}

fn default_schema() -> u32 {
    CLAUDE_ROSTER_SCHEMA_VERSION
}

/// Which harness-native items this roster picks up. All fields are
/// optional — a roster can restrict itself to, say, plugins only.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RosterSelection {
    pub plugins: Vec<String>,
    pub skills: Vec<String>,
    pub agents: Vec<String>,
    pub hooks: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub instructions: Vec<String>,
    pub output_styles: Vec<String>,
}

/// Per-invocation settings. Values mirror the Claude CLI flags and SDK
/// options; unknown keys are preserved via `extras` for forward-compat.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunProfile {
    pub backend: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub setting_sources: Vec<String>,
    pub output_format: Option<String>,
    pub include_partial_messages: Option<bool>,
    pub append_system_prompt: Option<String>,
    pub system_prompt: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u64>,
    #[serde(flatten)]
    pub extras: indexmap::IndexMap<String, toml::Value>,
}

/// Controls katachi's behavior, not Claude's.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RosterResolution {
    pub include_transitive: bool,
    pub materialization: MaterializationMode,
    pub strict_mcp_config: bool,
    pub bare: bool,
}

impl Default for RosterResolution {
    fn default() -> Self {
        Self {
            include_transitive: true,
            materialization: MaterializationMode::TempOverlay,
            strict_mcp_config: true,
            bare: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializationMode {
    Ambient,
    TempOverlay,
}

impl ClaudeRoster {
    /// Parse a Claude roster from a TOML string.
    pub fn from_toml_str(path: &Utf8Path, s: &str) -> Result<Self, ClaudeRosterError> {
        let parsed: ClaudeRoster =
            toml::from_str(s).map_err(|source| ClaudeRosterError::Parse {
                path: path.to_owned(),
                source,
            })?;
        parsed.validate(path)?;
        Ok(parsed)
    }

    /// Load a Claude roster from disk.
    pub fn from_file(path: &Utf8Path) -> Result<Self, ClaudeRosterError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ClaudeRosterError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str(path, &raw)
    }

    fn validate(&self, path: &Utf8Path) -> Result<(), ClaudeRosterError> {
        if self.version != CLAUDE_ROSTER_SCHEMA_VERSION {
            return Err(ClaudeRosterError::UnsupportedVersion {
                path: path.to_owned(),
                found: self.version,
                expected: CLAUDE_ROSTER_SCHEMA_VERSION,
            });
        }
        if self.id.trim().is_empty() {
            return Err(ClaudeRosterError::EmptyId {
                path: path.to_owned(),
            });
        }
        Ok(())
    }

    /// Materialization mode as its shared-core equivalent.
    pub fn materialization_mode(&self) -> katachi_core::model::MaterializationMode {
        match self.resolution.materialization {
            MaterializationMode::Ambient => katachi_core::model::MaterializationMode::Ambient,
            MaterializationMode::TempOverlay => {
                katachi_core::model::MaterializationMode::TempOverlay
            }
        }
    }

    /// Flatten the selection map into a `(kind, id)` list so callers can
    /// iterate without having to care about which sub-list an entry came
    /// from. Order matches the ordering in the roster TOML — plugins
    /// first, then skills, etc. — to keep downstream diagnostics stable.
    pub fn selection_entries(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        for p in &self.selection.plugins {
            out.push(("plugin", p.as_str()));
        }
        for s in &self.selection.skills {
            out.push(("skill", s.as_str()));
        }
        for a in &self.selection.agents {
            out.push(("agent", a.as_str()));
        }
        for h in &self.selection.hooks {
            out.push(("hook_set", h.as_str()));
        }
        for m in &self.selection.mcp_servers {
            out.push(("mcp_server", m.as_str()));
        }
        for i in &self.selection.instructions {
            out.push(("instruction_source", i.as_str()));
        }
        for s in &self.selection.output_styles {
            out.push(("output_style", s.as_str()));
        }
        out
    }

    /// Desired backend for this roster, if one is specified.
    pub fn backend(&self) -> Option<katachi_core::model::BackendKind> {
        self.run_profile
            .backend
            .as_deref()
            .and_then(|b| b.parse().ok())
    }

    /// Project the roster's selection into a [`katachi_core::selector::SelectorSet`]
    /// scoped to the Claude harness. `include_closure` mirrors
    /// [`RosterResolution::include_transitive`].
    pub fn to_selector_set(&self, include_closure: bool) -> katachi_core::selector::SelectorSet {
        use katachi_core::selector::{Selector, SelectorSet};
        let mut selectors: Vec<Selector> = Vec::new();
        let push = |out: &mut Vec<Selector>, kind: &str, ids: &[String]| {
            if ids.is_empty() {
                return;
            }
            out.push(Selector::ExplicitIds {
                kind: kind.to_owned(),
                ids: ids.to_vec(),
            });
        };
        push(&mut selectors, "plugin", &self.selection.plugins);
        push(&mut selectors, "skill", &self.selection.skills);
        push(&mut selectors, "agent", &self.selection.agents);
        push(&mut selectors, "hook_set", &self.selection.hooks);
        push(&mut selectors, "mcp_server", &self.selection.mcp_servers);
        push(
            &mut selectors,
            "instruction_source",
            &self.selection.instructions,
        );
        push(
            &mut selectors,
            "output_style",
            &self.selection.output_styles,
        );
        SelectorSet {
            selectors,
            include_packaging_closure: include_closure,
            include_semantic_closure: include_closure,
        }
    }

    /// Convert the run profile into a JSON overlay suitable for
    /// `KatachiTarget::run_profile_overlay`.
    pub fn run_profile_overlay(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        if let Some(v) = &self.run_profile.model {
            obj.insert("model".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.run_profile.permission_mode {
            obj.insert(
                "permission_mode".into(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.run_profile.output_format {
            obj.insert("output_format".into(), serde_json::Value::String(v.clone()));
        }
        if !self.run_profile.allowed_tools.is_empty() {
            obj.insert(
                "allowed_tools".into(),
                serde_json::Value::Array(
                    self.run_profile
                        .allowed_tools
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        if !self.run_profile.disallowed_tools.is_empty() {
            obj.insert(
                "disallowed_tools".into(),
                serde_json::Value::Array(
                    self.run_profile
                        .disallowed_tools
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        if !self.run_profile.setting_sources.is_empty() {
            obj.insert(
                "setting_sources".into(),
                serde_json::Value::Array(
                    self.run_profile
                        .setting_sources
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        if let Some(v) = self.run_profile.timeout_secs {
            obj.insert("timeout_secs".into(), serde_json::Value::Number(v.into()));
        }
        if obj.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(obj)
        }
    }
}

/// Conventional roster directory: `<data_root>/rosters/claude/` unless
/// the Claude config overrides it.
pub fn roster_dir(storage: &StoragePaths, config: &ClaudeConfig) -> Utf8PathBuf {
    if let Some(custom) = &config.roster_dir_override {
        custom.clone()
    } else {
        storage.rosters_dir().join("claude")
    }
}

/// Flat-file store that loads every `*.toml` under a directory into
/// [`ClaudeRoster`] values.
#[derive(Clone, Debug, Default)]
pub struct ClaudeRosterStore {
    rosters: Vec<ClaudeRoster>,
    source_dir: Option<Utf8PathBuf>,
}

impl ClaudeRosterStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rosters<I: IntoIterator<Item = ClaudeRoster>>(iter: I) -> Self {
        Self {
            rosters: iter.into_iter().collect(),
            source_dir: None,
        }
    }

    /// Load every `*.toml` under `dir`, sorted by id. Missing directories
    /// yield an empty store so the caller can decide whether this is a
    /// diagnostic or a fatal error.
    pub fn load_from_dir(dir: &Utf8Path) -> Result<Self, ClaudeRosterError> {
        let mut store = Self::new();
        store.source_dir = Some(dir.to_owned());
        if !dir.exists() {
            return Ok(store);
        }
        let mut seen: std::collections::BTreeMap<String, Utf8PathBuf> =
            std::collections::BTreeMap::new();
        let read =
            std::fs::read_dir(dir.as_std_path()).map_err(|source| ClaudeRosterError::ReadDir {
                path: dir.to_owned(),
                source,
            })?;
        for entry in read {
            let entry = entry.map_err(|source| ClaudeRosterError::ReadDir {
                path: dir.to_owned(),
                source,
            })?;
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(utf8) = Utf8PathBuf::from_path_buf(p).ok() else {
                continue;
            };
            if utf8.extension() != Some("toml") {
                continue;
            }
            let r = ClaudeRoster::from_file(&utf8)?;
            if let Some(prev) = seen.get(&r.id) {
                return Err(ClaudeRosterError::DuplicateId {
                    id: r.id.clone(),
                    first: prev.clone(),
                    second: utf8,
                });
            }
            seen.insert(r.id.clone(), utf8.clone());
            store.rosters.push(r);
        }
        store.rosters.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(store)
    }

    /// Convenience: load the store from the conventional
    /// `<data_root>/rosters/claude/` directory.
    pub fn load_default(
        storage: &StoragePaths,
        config: &ClaudeConfig,
    ) -> Result<Self, ClaudeRosterError> {
        Self::load_from_dir(&roster_dir(storage, config))
    }

    /// Fetch a roster by id or fail with `NotFound`.
    pub fn require(&self, id: &str) -> Result<&ClaudeRoster, ClaudeRosterError> {
        self.find(id).ok_or_else(|| ClaudeRosterError::NotFound {
            id: id.to_string(),
            dir: self
                .source_dir
                .clone()
                .unwrap_or_else(|| Utf8PathBuf::from("<in-memory>")),
        })
    }

    /// The directory the store was loaded from, if any.
    pub fn source_dir(&self) -> Option<&Utf8Path> {
        self.source_dir.as_deref()
    }

    pub fn find(&self, id: &str) -> Option<&ClaudeRoster> {
        self.rosters.iter().find(|r| r.id == id)
    }

    pub fn all(&self) -> &[ClaudeRoster] {
        &self.rosters
    }

    pub fn is_empty(&self) -> bool {
        self.rosters.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rosters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const REALISTIC: &str = r#"
version = 1
id = "accessibility-auditor"
description = "Claude accessibility audit loadout"

[selection]
plugins = ["web-a11y"]
skills = ["axe-runner"]
agents = ["a11y-reviewer"]
hooks = ["a11y-report-hooks"]
mcp_servers = ["chrome-devtools"]
instructions = ["project:CLAUDE.md", "rule:a11y-review"]
output_styles = []

[run_profile]
backend = "cli"
model = "sonnet"
permission_mode = "plan"
setting_sources = ["project", "user"]
output_format = "stream-json"
include_partial_messages = true
append_system_prompt = "Focus on WCAG 2.2 AA issues and produce a concise findings list."

[resolution]
include_transitive = true
materialization = "temp-overlay"
strict_mcp_config = true
bare = false
"#;

    #[test]
    fn realistic_roster_parses() {
        let path = Utf8PathBuf::from("realistic.toml");
        let r = ClaudeRoster::from_toml_str(&path, REALISTIC).unwrap();
        assert_eq!(r.id, "accessibility-auditor");
        assert_eq!(r.selection.plugins, vec!["web-a11y".to_string()]);
        assert_eq!(r.run_profile.model.as_deref(), Some("sonnet"));
        assert_eq!(r.run_profile.setting_sources.len(), 2);
        assert_eq!(
            r.resolution.materialization,
            MaterializationMode::TempOverlay
        );
    }

    #[test]
    fn empty_id_rejected() {
        let path = Utf8PathBuf::from("bad.toml");
        let bad = r#"
version = 1
id = ""
"#;
        let err = ClaudeRoster::from_toml_str(&path, bad).unwrap_err();
        assert!(matches!(err, ClaudeRosterError::EmptyId { .. }));
    }

    #[test]
    fn unknown_version_rejected() {
        let path = Utf8PathBuf::from("bad.toml");
        let bad = r#"
version = 99
id = "x"
"#;
        let err = ClaudeRoster::from_toml_str(&path, bad).unwrap_err();
        assert!(matches!(err, ClaudeRosterError::UnsupportedVersion { .. }));
    }

    #[test]
    fn store_loads_directory_sorted() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        std::fs::write(
            root.join("zeta.toml"),
            r#"
version = 1
id = "zeta"
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("alpha.toml"),
            r#"
version = 1
id = "alpha"
"#,
        )
        .unwrap();
        std::fs::write(root.join("notes.md"), "ignored").unwrap();

        let store = ClaudeRosterStore::load_from_dir(&root).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.all()[0].id, "alpha");
        assert_eq!(store.all()[1].id, "zeta");
        assert!(store.find("alpha").is_some());
        assert!(store.find("ghost").is_none());
    }

    #[test]
    fn defaults_applied_when_sections_omitted() {
        let path = Utf8PathBuf::from("minimal.toml");
        let minimal = r#"
version = 1
id = "min"
"#;
        let r = ClaudeRoster::from_toml_str(&path, minimal).unwrap();
        assert!(r.selection.plugins.is_empty());
        assert!(r.run_profile.setting_sources.is_empty());
        assert_eq!(
            r.resolution.materialization,
            MaterializationMode::TempOverlay
        );
    }

    #[test]
    fn require_errors_when_missing() {
        let store = ClaudeRosterStore::from_rosters(std::iter::empty());
        let err = store.require("ghost").unwrap_err();
        assert!(matches!(err, ClaudeRosterError::NotFound { .. }));
    }

    #[test]
    fn selection_entries_flattens_in_stable_order() {
        let path = Utf8PathBuf::from("t.toml");
        let r = ClaudeRoster::from_toml_str(&path, REALISTIC).unwrap();
        let entries = r.selection_entries();
        let kinds: Vec<_> = entries.iter().map(|(k, _)| *k).collect();
        // Plugins come before skills which come before agents, etc.
        assert!(
            kinds.iter().position(|k| *k == "plugin") < kinds.iter().position(|k| *k == "skill")
        );
        assert!(
            kinds.iter().position(|k| *k == "skill") < kinds.iter().position(|k| *k == "agent")
        );
    }

    #[test]
    fn backend_parses_to_kind() {
        let path = Utf8PathBuf::from("t.toml");
        let r = ClaudeRoster::from_toml_str(&path, REALISTIC).unwrap();
        assert_eq!(r.backend(), Some(katachi_core::model::BackendKind::Cli));
    }
}
