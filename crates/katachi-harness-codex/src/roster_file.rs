//! Codex roster file format (`rosters/codex/<id>.toml`).
//!
//! The roster file declares which discovered items a Codex katachi
//! selects, plus the run profile (model, approval/sandbox, output mode)
//! and the resolution policy (closure, materialization, trust).

use camino::{Utf8Path, Utf8PathBuf};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::items::CodexItemKind;

pub const ROSTER_SCHEMA_VERSION: u32 = 1;

/// Top-level roster file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexRosterFile {
    #[serde(default = "default_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub selection: Selection,
    #[serde(default)]
    pub run_profile: RunProfile,
    #[serde(default)]
    pub resolution: Resolution,
}

fn default_version() -> u32 {
    ROSTER_SCHEMA_VERSION
}

/// Everything the roster should pull in by id. Each list maps onto a
/// [`CodexItemKind`] and is resolved to fully-qualified `ItemRef`s against
/// the discovered catalog.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub config_layers: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub instructions: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.config_layers.is_empty()
            && self.profiles.is_empty()
            && self.instructions.is_empty()
            && self.skills.is_empty()
            && self.agents.is_empty()
            && self.hooks.is_empty()
            && self.mcp_servers.is_empty()
            && self.rules.is_empty()
            && self.plugins.is_empty()
    }

    /// Group selections by `CodexItemKind` so callers can iterate uniformly.
    pub fn by_kind(&self) -> IndexMap<CodexItemKind, Vec<String>> {
        let mut out: IndexMap<CodexItemKind, Vec<String>> = IndexMap::new();
        out.insert(CodexItemKind::ConfigLayer, self.config_layers.clone());
        out.insert(CodexItemKind::Profile, self.profiles.clone());
        out.insert(CodexItemKind::InstructionDoc, self.instructions.clone());
        out.insert(CodexItemKind::Skill, self.skills.clone());
        out.insert(CodexItemKind::CustomAgent, self.agents.clone());
        out.insert(CodexItemKind::HookSet, self.hooks.clone());
        out.insert(CodexItemKind::McpServer, self.mcp_servers.clone());
        out.insert(CodexItemKind::RuleSet, self.rules.clone());
        out.insert(CodexItemKind::Plugin, self.plugins.clone());
        out
    }
}

/// Per-invocation run profile.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunProfile {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub approval_policy: Option<String>,
    #[serde(default)]
    pub sandbox_mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub output_mode: Option<String>,
    #[serde(default)]
    pub output_schema_file: Option<Utf8PathBuf>,
    #[serde(default)]
    pub writable_dirs: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Anything else is preserved as raw TOML for passthrough overrides.
    #[serde(default, flatten)]
    pub extra: IndexMap<String, toml::Value>,
}

/// Resolution-time policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolution {
    #[serde(default = "default_true")]
    pub include_transitive: bool,
    #[serde(default = "default_materialization")]
    pub materialization: String,
    #[serde(default = "default_true")]
    pub respect_project_trust: bool,
}

impl Default for Resolution {
    fn default() -> Self {
        Self {
            include_transitive: true,
            materialization: default_materialization(),
            respect_project_trust: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_materialization() -> String {
    "temp-overlay".into()
}

impl CodexRosterFile {
    pub fn from_toml_str(s: &str) -> Result<Self, RosterFileError> {
        let roster: Self = toml::from_str(s).map_err(RosterFileError::Parse)?;
        roster.validate()?;
        Ok(roster)
    }

    pub fn from_toml_file(path: &Utf8Path) -> Result<Self, RosterFileError> {
        let raw = std::fs::read_to_string(path).map_err(|source| RosterFileError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str(&raw)
    }

    /// Project the selection into a [`katachi_core::selector::SelectorSet`]
    /// scoped to the Codex harness.
    pub fn to_selector_set(
        &self,
        include_closure: bool,
    ) -> katachi_core::selector::SelectorSet {
        use katachi_core::selector::{Selector, SelectorSet};
        let mut selectors: Vec<Selector> = Vec::new();
        for (kind, ids) in self.selection.by_kind() {
            if ids.is_empty() {
                continue;
            }
            selectors.push(Selector::ExplicitIds {
                kind: kind.as_str().to_owned(),
                ids,
            });
        }
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
        if let Some(v) = &self.run_profile.approval_policy {
            obj.insert(
                "approval_policy".into(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.run_profile.sandbox_mode {
            obj.insert(
                "sandbox_mode".into(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.run_profile.model {
            obj.insert("model".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.run_profile.profile {
            obj.insert("profile".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.run_profile.output_mode {
            obj.insert("output_mode".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = self.run_profile.timeout_secs {
            obj.insert(
                "timeout_secs".into(),
                serde_json::Value::Number(v.into()),
            );
        }
        if obj.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(obj)
        }
    }

    fn validate(&self) -> Result<(), RosterFileError> {
        if self.version != ROSTER_SCHEMA_VERSION {
            return Err(RosterFileError::UnsupportedSchema {
                found: self.version,
                expected: ROSTER_SCHEMA_VERSION,
            });
        }
        if self.id.trim().is_empty() {
            return Err(RosterFileError::EmptyId);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RosterFileError {
    #[error("failed to parse roster TOML: {0}")]
    Parse(#[source] toml::de::Error),
    #[error("failed to read roster file {path}: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported roster schema {found} (expected {expected})")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("roster id must not be empty")]
    EmptyId,
    #[error("duplicate codex roster id `{id}` in `{first}` and `{second}`")]
    DuplicateId {
        id: String,
        first: Utf8PathBuf,
        second: Utf8PathBuf,
    },
}

/// Load every `*.toml` under `dir` as a [`CodexRosterFile`].
pub fn load_rosters_dir(dir: &Utf8Path) -> Result<Vec<CodexRosterFile>, RosterFileError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(dir.as_std_path()).map_err(|source| RosterFileError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut out: Vec<CodexRosterFile> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, Utf8PathBuf> =
        std::collections::BTreeMap::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(utf8) = Utf8PathBuf::from_path_buf(path) else {
            continue;
        };
        if utf8.extension() != Some("toml") {
            continue;
        }
        let parsed = CodexRosterFile::from_toml_file(&utf8)?;
        if let Some(prev) = seen.get(&parsed.id) {
            return Err(RosterFileError::DuplicateId {
                id: parsed.id.clone(),
                first: prev.clone(),
                second: utf8,
            });
        }
        seen.insert(parsed.id.clone(), utf8.clone());
        out.push(parsed);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
version = 1
id = "accessibility-auditor"
description = "Codex accessibility audit loadout"

[selection]
profiles = ["review"]
instructions = ["global:AGENTS.md", "project:AGENTS.md"]
skills = ["accessibility-audit"]
agents = ["readonly-reviewer"]
hooks = ["reporting-hooks"]
mcp_servers = ["chrome-devtools", "openaiDeveloperDocs"]
rules = ["readonly-shell"]
plugins = []

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
profile = "review"
output_mode = "machine-readable"
output_schema_file = "schemas/a11y-report.json"

[resolution]
include_transitive = true
materialization = "temp-overlay"
respect_project_trust = true
"#;

    #[test]
    fn parses_sample_roster() {
        let rf = CodexRosterFile::from_toml_str(SAMPLE).unwrap();
        assert_eq!(rf.id, "accessibility-auditor");
        assert_eq!(rf.selection.profiles, vec!["review"]);
        assert_eq!(rf.selection.mcp_servers.len(), 2);
        assert_eq!(rf.run_profile.approval_policy.as_deref(), Some("never"));
        assert_eq!(
            rf.run_profile.output_schema_file.as_ref().unwrap().as_str(),
            "schemas/a11y-report.json"
        );
        assert!(rf.resolution.respect_project_trust);
    }

    #[test]
    fn empty_id_rejected() {
        let err = CodexRosterFile::from_toml_str(
            r#"
version = 1
id = ""
"#,
        )
        .unwrap_err();
        assert!(matches!(err, RosterFileError::EmptyId));
    }

    #[test]
    fn unsupported_schema_rejected() {
        let err = CodexRosterFile::from_toml_str(
            r#"
version = 99
id = "x"
"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RosterFileError::UnsupportedSchema {
                found: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn by_kind_groups_by_codex_item_kind() {
        let rf = CodexRosterFile::from_toml_str(SAMPLE).unwrap();
        let grouped = rf.selection.by_kind();
        assert_eq!(
            grouped.get(&CodexItemKind::Skill).unwrap(),
            &vec!["accessibility-audit"]
        );
        assert_eq!(
            grouped.get(&CodexItemKind::McpServer).unwrap().len(),
            2
        );
    }

    #[test]
    fn missing_sections_default_sensibly() {
        let rf = CodexRosterFile::from_toml_str(
            r#"
version = 1
id = "minimal"
"#,
        )
        .unwrap();
        assert!(rf.selection.is_empty());
        assert!(rf.resolution.respect_project_trust);
        assert_eq!(rf.resolution.materialization, "temp-overlay");
    }

    #[test]
    fn extras_preserved_on_run_profile() {
        let rf = CodexRosterFile::from_toml_str(
            r#"
version = 1
id = "x"
[run_profile]
model = "gpt-5.4"
custom_key = "value"
"#,
        )
        .unwrap();
        assert!(rf.run_profile.extra.contains_key("custom_key"));
    }
}
