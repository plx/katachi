//! Gemini roster file parser.
//!
//! Loads `rosters/gemini/*.toml` into typed [`GeminiRoster`] values. A
//! roster combines a *selection* (which extensions, contexts, skills,
//! subagents, hooks, MCP servers, and policies to pull in) with a
//! *run_profile* (how to invoke the CLI/SDK) and *resolution* flags
//! (closure/materialization/preview requirements).

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use katachi_core::katachi::{KatachiDefinition, KatachiTarget, KATACHI_SCHEMA_VERSION};
use katachi_core::model::{BackendKind, HarnessKind, ItemRef};
use katachi_core::selector::{Selector, SelectorSet};

use crate::item::GeminiItemKind;

/// A loaded Gemini roster definition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiRoster {
    #[serde(default = "default_schema")]
    pub version: u32,
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub selection: RosterSelection,
    #[serde(default)]
    pub run_profile: RosterRunProfile,
    #[serde(default)]
    pub resolution: ResolutionFlags,
}

fn default_schema() -> u32 {
    1
}

/// Selection block. Each vector is a list of item ids, matched against
/// whatever the Gemini scanner produced for that kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RosterSelection {
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Context selectors are tagged strings like `project:GEMINI.md`,
    /// `user:GEMINI.md`, or `extension:workspace-a11y:GEMINI.md`.
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub subagents: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub policies: Vec<String>,
}

impl RosterSelection {
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
            && self.context.is_empty()
            && self.skills.is_empty()
            && self.subagents.is_empty()
            && self.hooks.is_empty()
            && self.mcp_servers.is_empty()
            && self.policies.is_empty()
    }

    /// Turn this selection into a `SelectorSet` targeting Gemini.
    pub fn to_selector_set(&self, include_closure: bool) -> SelectorSet {
        let mut selectors = Vec::new();
        push_explicit(&mut selectors, GeminiItemKind::Extension, &self.extensions);
        push_context_selectors(&mut selectors, &self.context);
        push_explicit(&mut selectors, GeminiItemKind::Skill, &self.skills);
        push_explicit(&mut selectors, GeminiItemKind::Subagent, &self.subagents);
        push_explicit(&mut selectors, GeminiItemKind::HookSet, &self.hooks);
        // MCP ids from a roster file usually don't encode scope, so match
        // against settings- and extension-sourced mcp items.
        for name in &self.mcp_servers {
            push_mcp_selectors(&mut selectors, name);
        }
        push_explicit(&mut selectors, GeminiItemKind::PolicySet, &self.policies);
        SelectorSet {
            selectors,
            include_packaging_closure: include_closure,
            include_semantic_closure: include_closure,
        }
    }
}

fn push_explicit(out: &mut Vec<Selector>, kind: GeminiItemKind, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    out.push(Selector::ExplicitIds {
        kind: kind.as_str().to_owned(),
        ids: ids.to_vec(),
    });
}

fn push_context_selectors(out: &mut Vec<Selector>, entries: &[String]) {
    for entry in entries {
        // Entries are `scope:file` (e.g. `project:GEMINI.md`,
        // `extension:workspace-a11y:GEMINI.md`). We map them onto the
        // corresponding `ContextSource` id format.
        let id = match entry.split(':').count() {
            2 => {
                // scope:file -> context:<scope>:<file>
                let mut it = entry.splitn(2, ':');
                let scope = it.next().unwrap();
                let file = it.next().unwrap();
                format!("context:{scope}:{file}")
            }
            _ => {
                // extension:<name>:<file> -> context:extension:<name>:<file>
                format!("context:{entry}")
            }
        };
        out.push(Selector::ItemRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::ContextSource.as_str(),
                id,
            ),
        });
    }
}

fn push_mcp_selectors(out: &mut Vec<Selector>, name: &str) {
    // Try all scopes; the glob matches the ID suffix.
    let pattern = format!("*:{name}");
    out.push(Selector::Glob {
        kind: Some(GeminiItemKind::McpServer.as_str().to_owned()),
        pattern,
    });
}

/// Per-invocation behavior.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RosterRunProfile {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub approval_mode: Option<String>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub include_directories: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub extensions_mode: Option<String>,
    #[serde(default)]
    pub extra_flags: Vec<String>,
    /// Override the binary invoked by the planner. Primarily used by
    /// tests (fake gemini scripts) but also valid for users with a
    /// custom binary path.
    #[serde(default)]
    pub binary: Option<String>,
}

impl RosterRunProfile {
    /// Produce a serde_json value suitable for a `KatachiTarget.run_profile_overlay`.
    pub fn as_overlay(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(v) = &self.model {
            m.insert("model".into(), Value::String(v.clone()));
        }
        if let Some(v) = &self.approval_mode {
            m.insert("approval_mode".into(), Value::String(v.clone()));
        }
        if let Some(v) = &self.output_format {
            m.insert("output_format".into(), Value::String(v.clone()));
        }
        if !self.include_directories.is_empty() {
            m.insert(
                "include_directories".into(),
                Value::Array(
                    self.include_directories
                        .iter()
                        .map(|p| Value::String(p.to_string()))
                        .collect(),
                ),
            );
        }
        if let Some(v) = &self.extensions_mode {
            m.insert("extensions_mode".into(), Value::String(v.clone()));
        }
        if !self.extra_flags.is_empty() {
            m.insert(
                "extra_flags".into(),
                Value::Array(
                    self.extra_flags
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(v) = &self.binary {
            m.insert("binary".into(), Value::String(v.clone()));
        }
        Value::Object(m)
    }

    pub fn backend_kind(&self) -> Option<BackendKind> {
        self.backend.as_deref().and_then(|s| s.parse().ok())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolutionFlags {
    #[serde(default = "default_true")]
    pub include_transitive: bool,
    #[serde(default = "default_materialization")]
    pub materialization: String,
    #[serde(default)]
    pub require_preview_features: bool,
}

impl Default for ResolutionFlags {
    fn default() -> Self {
        Self {
            include_transitive: true,
            materialization: default_materialization(),
            require_preview_features: false,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_materialization() -> String {
    "temp-overlay".to_owned()
}

impl GeminiRoster {
    pub fn from_toml_str(s: &str) -> Result<Self, RosterError> {
        toml::from_str::<Self>(s).map_err(RosterError::Parse)
    }

    pub fn from_toml_file(path: &Utf8Path) -> Result<Self, RosterError> {
        let raw = fs::read_to_string(path.as_std_path()).map_err(|source| RosterError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str(&raw)
    }

    /// Project this roster into a [`KatachiDefinition`] suitable for the
    /// shared resolver.
    pub fn to_katachi_definition(&self) -> KatachiDefinition {
        let target = KatachiTarget {
            harness: HarnessKind::Gemini,
            roster_id: Some(self.id.clone()),
            backend: self.run_profile.backend_kind(),
            preference: 0,
            selectors: self
                .selection
                .to_selector_set(self.resolution.include_transitive),
            run_profile_overlay: self.run_profile.as_overlay(),
        };
        KatachiDefinition {
            schema_version: KATACHI_SCHEMA_VERSION,
            id: self.id.clone(),
            description: self.description.clone(),
            targets: vec![target],
        }
    }
}

/// Store of Gemini rosters loaded from a directory.
#[derive(Clone, Debug, Default)]
pub struct GeminiRosterStore {
    rosters: Vec<GeminiRoster>,
}

impl GeminiRosterStore {
    pub fn load_dir(dir: &Utf8Path) -> Result<Self, RosterError> {
        if !dir.exists() {
            return Ok(Self::default());
        }
        let mut out = Self::default();
        let mut seen: std::collections::BTreeMap<String, Utf8PathBuf> =
            std::collections::BTreeMap::new();
        let iter = fs::read_dir(dir.as_std_path()).map_err(|source| RosterError::Io {
            path: dir.to_owned(),
            source,
        })?;
        for entry in iter {
            let entry = entry.map_err(|source| RosterError::Io {
                path: dir.to_owned(),
                source,
            })?;
            let Ok(utf8) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            if utf8.extension() != Some("toml") {
                continue;
            }
            let roster = GeminiRoster::from_toml_file(&utf8)?;
            if let Some(prev) = seen.get(&roster.id) {
                return Err(RosterError::DuplicateId {
                    id: roster.id.clone(),
                    first: prev.clone(),
                    second: utf8,
                });
            }
            seen.insert(roster.id.clone(), utf8.clone());
            out.rosters.push(roster);
        }
        out.rosters.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn find(&self, id: &str) -> Option<&GeminiRoster> {
        self.rosters.iter().find(|r| r.id == id)
    }

    pub fn all(&self) -> &[GeminiRoster] {
        &self.rosters
    }

    pub fn is_empty(&self) -> bool {
        self.rosters.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum RosterError {
    #[error("failed to read roster `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse Gemini roster TOML: {0}")]
    Parse(#[source] toml::de::Error),
    #[error("duplicate gemini roster id `{id}` in `{first}` and `{second}`")]
    DuplicateId {
        id: String,
        first: Utf8PathBuf,
        second: Utf8PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const ACCESSIBILITY_ROSTER: &str = r#"
version = 1
id = "accessibility-auditor"
description = "Gemini accessibility audit loadout"

[selection]
extensions = ["workspace-a11y"]
context = ["project:GEMINI.md"]
skills = ["accessibility-audit"]
subagents = ["codebase_investigator"]
hooks = ["a11y-hooks"]
mcp_servers = ["chrome-devtools"]
policies = ["readonly-audit"]

[run_profile]
backend = "cli"
model = "gemini-3-pro-preview"
approval_mode = "plan"
output_format = "stream-json"
include_directories = ["docs", "apps/web"]
extensions_mode = "selected-only"

[resolution]
include_transitive = true
materialization = "temp-overlay"
require_preview_features = true
"#;

    #[test]
    fn parses_realistic_roster() {
        let r = GeminiRoster::from_toml_str(ACCESSIBILITY_ROSTER).unwrap();
        assert_eq!(r.id, "accessibility-auditor");
        assert_eq!(r.selection.extensions, vec!["workspace-a11y"]);
        assert_eq!(
            r.run_profile.model.as_deref(),
            Some("gemini-3-pro-preview")
        );
        assert!(r.resolution.require_preview_features);
    }

    #[test]
    fn selection_to_selector_set_includes_extension_kind() {
        let r = GeminiRoster::from_toml_str(ACCESSIBILITY_ROSTER).unwrap();
        let sel = r.selection.to_selector_set(true);
        let found = sel.selectors.iter().any(|s| matches!(s, Selector::ExplicitIds { kind, .. } if kind == "extension"));
        assert!(found);
        assert!(sel.include_packaging_closure);
    }

    #[test]
    fn to_katachi_definition_embeds_overlay() {
        let r = GeminiRoster::from_toml_str(ACCESSIBILITY_ROSTER).unwrap();
        let def = r.to_katachi_definition();
        assert_eq!(def.targets.len(), 1);
        let tgt = &def.targets[0];
        assert_eq!(tgt.harness, HarnessKind::Gemini);
        assert_eq!(tgt.backend, Some(BackendKind::Cli));
        assert!(tgt.run_profile_overlay.is_object());
        assert_eq!(tgt.run_profile_overlay["model"], "gemini-3-pro-preview");
        assert_eq!(tgt.run_profile_overlay["approval_mode"], "plan");
    }

    #[test]
    fn context_selector_encoding() {
        let sel = RosterSelection {
            context: vec![
                "project:GEMINI.md".into(),
                "extension:workspace-a11y:GEMINI.md".into(),
            ],
            ..Default::default()
        };
        let set = sel.to_selector_set(true);
        let refs: Vec<&ItemRef> = set
            .selectors
            .iter()
            .filter_map(|s| match s {
                Selector::ItemRef { item_ref } => Some(item_ref),
                _ => None,
            })
            .collect();
        assert!(refs
            .iter()
            .any(|r| r.id == "context:project:GEMINI.md"));
        assert!(refs
            .iter()
            .any(|r| r.id == "context:extension:workspace-a11y:GEMINI.md"));
    }

    #[test]
    fn mcp_selector_uses_glob() {
        let sel = RosterSelection {
            mcp_servers: vec!["chrome-devtools".into()],
            ..Default::default()
        };
        let set = sel.to_selector_set(true);
        let pattern_found = set.selectors.iter().any(|s| match s {
            Selector::Glob { kind, pattern } => {
                kind.as_deref() == Some("mcp_server") && pattern.ends_with(":chrome-devtools")
            }
            _ => false,
        });
        assert!(pattern_found);
    }

    #[test]
    fn store_loads_dir_sorted_and_finds_id() {
        let tmp = TempDir::new().unwrap();
        let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::write(
            dir.join("beta.toml").as_std_path(),
            r#"
id = "beta"

[selection]
extensions = ["demo"]
"#,
        )
        .unwrap();
        fs::write(
            dir.join("alpha.toml").as_std_path(),
            r#"
id = "alpha"
"#,
        )
        .unwrap();
        let store = GeminiRosterStore::load_dir(&dir).unwrap();
        assert!(!store.is_empty());
        assert!(store.find("alpha").is_some());
        // Order sorted by id.
        assert_eq!(store.all()[0].id, "alpha");
    }

    #[test]
    fn missing_dir_is_empty_store() {
        let dir = Utf8PathBuf::from("/no/such/dir/ever");
        let store = GeminiRosterStore::load_dir(&dir).unwrap();
        assert!(store.is_empty());
    }
}
