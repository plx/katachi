//! Codex custom-agent discovery.
//!
//! Agents live in:
//!
//! - `$CODEX_HOME/agents/<id>/agent.toml` (or `agent.yaml`)
//! - `<project>/.codex/agents/<id>/agent.toml`
//!
//! The raw agent config is preserved so later stages can read fields like
//! `model`, `approval_policy`, `inherits`, etc. without this module
//! committing to a specific schema.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};

use crate::config_layers::ConfigLayer;
use crate::items::{CodexEdgeKind, CodexItemKind};
use crate::CodexSettings;

/// A discovered custom agent.
#[derive(Clone, Debug)]
pub struct CustomAgent {
    pub id: String,
    pub path: Utf8PathBuf,
    pub scope: String,
    pub raw: serde_json::Value,
    /// The layer this agent's config lives near — used to build
    /// `AgentUsesConfigLayer` edges.
    pub parent_layer_id: Option<String>,
    /// Inherited-from session is inferred when no `config_layer` is set in
    /// the agent config: the agent picks up the enclosing session defaults.
    pub inherits_session: bool,
}

impl CustomAgent {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(
            HarnessKind::Codex,
            CodexItemKind::CustomAgent.as_str(),
            &self.id,
        )
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "scope": self.scope,
            "path": self.path.as_str(),
            "config": self.raw,
            "parent_layer_id": self.parent_layer_id,
            "inherits_session": self.inherits_session,
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.id.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(self.scope.clone()),
                provenance: Some("codex.custom_agent".into()),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// Scan home and project `.codex/agents/<id>/agent.*` files.
pub fn discover_agents(
    settings: &CodexSettings,
    project_roots: &[Utf8PathBuf],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<CustomAgent> {
    let mut agents: Vec<CustomAgent> = Vec::new();

    let user_dir = settings.codex_home.join("agents");
    if user_dir.is_dir() {
        collect_agents(&user_dir, "user", &mut agents, diagnostics);
    }

    for root in project_roots {
        let project_dir = root.join(".codex").join("agents");
        if project_dir.is_dir() {
            collect_agents(&project_dir, "project", &mut agents, diagnostics);
        }
    }

    agents.sort_by(|a, b| a.id.cmp(&b.id));
    agents
}

/// Insert `AgentUsesConfigLayer` and `AgentInheritsSessionDefaults` edges.
pub fn insert_agent_edges(
    agents: &[CustomAgent],
    layers: &[ConfigLayer],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for agent in agents {
        if let Some(layer_id) = &agent.parent_layer_id {
            if let Some(layer) = layers.iter().find(|l| &l.id == layer_id) {
                let edge = DependencyEdge {
                    from: agent.item_ref(),
                    to: layer.item_ref(),
                    kind: EdgeKind::Semantic,
                    required: false,
                    note: Some(CodexEdgeKind::AgentUsesConfigLayer.as_str().to_string()),
                };
                insert_edge(catalog, edge, diagnostics);
            }
        }
        if agent.inherits_session {
            // There's no explicit "session" node; we model inheritance as a
            // self-loop marker note on the item_ref <-> nearest layer edge.
            if let Some(nearest) = agents_nearest_layer(agent, layers) {
                let edge = DependencyEdge {
                    from: agent.item_ref(),
                    to: nearest,
                    kind: EdgeKind::Semantic,
                    required: false,
                    note: Some(
                        CodexEdgeKind::AgentInheritsSessionDefaults
                            .as_str()
                            .to_string(),
                    ),
                };
                insert_edge(catalog, edge, diagnostics);
            }
        }
    }
}

fn agents_nearest_layer(agent: &CustomAgent, layers: &[ConfigLayer]) -> Option<ItemRef> {
    // If the agent sits under a project `.codex/agents/`, try to link it
    // against the project's config layer (if any was discovered).
    if agent.scope == "project" {
        for layer in layers.iter().rev() {
            if layer.source == crate::config_layers::ConfigSource::Project {
                return Some(layer.item_ref());
            }
        }
    }
    layers
        .iter()
        .rev()
        .find(|l| l.source == crate::config_layers::ConfigSource::User)
        .map(ConfigLayer::item_ref)
}

fn insert_edge(
    catalog: &mut RosterCatalog,
    edge: DependencyEdge,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match catalog.insert_edge(edge) {
        Ok(()) | Err(RosterBuildError::DuplicateEdge { .. }) => {}
        Err(err) => diagnostics.push(Diagnostic::warning(
            "codex.agent.edge",
            format!("failed to insert agent edge: {err}"),
        )),
    }
}

fn collect_agents(
    dir: &Utf8Path,
    scope: &str,
    out: &mut Vec<CustomAgent>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.agents.read-dir",
                format!("failed to read agents dir `{dir}`: {err}"),
            ));
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(agent_dir) = Utf8PathBuf::from_path_buf(path) else {
            continue;
        };
        if let Some(agent) = load_agent(&agent_dir, scope, diagnostics) {
            out.push(agent);
        }
    }
}

fn load_agent(
    dir: &Utf8Path,
    scope: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<CustomAgent> {
    let toml_path = dir.join("agent.toml");
    if toml_path.is_file() {
        return load_agent_toml(dir, &toml_path, scope, diagnostics);
    }
    // Accept `.yaml` as fallback; we preserve it as raw text in
    // `raw.source_yaml` so operators can inspect it even though we don't
    // parse YAML in v1.
    for ext in ["agent.yaml", "agent.yml"] {
        let alt = dir.join(ext);
        if alt.is_file() {
            return load_agent_yaml(dir, &alt, scope, diagnostics);
        }
    }
    None
}

fn load_agent_toml(
    dir: &Utf8Path,
    file: &Utf8Path,
    scope: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<CustomAgent> {
    let raw = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.agent.read",
                format!("failed to read `{file}`: {err}"),
            ));
            return None;
        }
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            diagnostics.push(Diagnostic::error(
                "codex.agent.parse",
                format!("failed to parse `{file}`: {err}"),
            ));
            return None;
        }
    };
    let json = toml_to_json(&parsed);
    let parent_layer_id = json
        .get("config_layer")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let inherits_session = json
        .get("inherit_session")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Some(CustomAgent {
        id: dir.file_name().unwrap_or("unnamed").to_string(),
        path: file.to_path_buf(),
        scope: scope.to_string(),
        raw: json,
        parent_layer_id,
        inherits_session,
    })
}

fn load_agent_yaml(
    dir: &Utf8Path,
    file: &Utf8Path,
    scope: &str,
    _diagnostics: &mut Vec<Diagnostic>,
) -> Option<CustomAgent> {
    let raw = std::fs::read_to_string(file).ok()?;
    Some(CustomAgent {
        id: dir.file_name().unwrap_or("unnamed").to_string(),
        path: file.to_path_buf(),
        scope: scope.to_string(),
        raw: serde_json::json!({
            "source": "yaml",
            "body": raw,
        }),
        parent_layer_id: None,
        inherits_session: true,
    })
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(tbl) => {
            let mut map = serde_json::Map::new();
            for (k, v) in tbl {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &Utf8Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn td_utf8() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().unwrap();
        let p = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        (td, p)
    }

    #[test]
    fn agent_toml_parsed_and_fields_preserved() {
        let (_td, root) = td_utf8();
        write(
            &root.join(".codex/agents/readonly/agent.toml"),
            r#"
model = "gpt-5.4"
approval_policy = "never"
config_layer = "project:/r"
inherit_session = false
"#,
        );
        let settings = CodexSettings {
            codex_home: root.join("absent"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let agents = discover_agents(&settings, std::slice::from_ref(&root), &mut diags);
        assert_eq!(agents.len(), 1);
        let a = &agents[0];
        assert_eq!(a.id, "readonly");
        assert_eq!(a.raw["model"], "gpt-5.4");
        assert_eq!(a.parent_layer_id.as_deref(), Some("project:/r"));
        assert!(!a.inherits_session);
    }

    #[test]
    fn yaml_fallback_preserves_raw_body() {
        let (_td, root) = td_utf8();
        write(
            &root.join(".codex/agents/fixer/agent.yaml"),
            "model: gpt-5.4\napproval_policy: on-request\n",
        );
        let settings = CodexSettings {
            codex_home: root.join("absent"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let agents = discover_agents(&settings, std::slice::from_ref(&root), &mut diags);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].raw["source"], "yaml");
        assert!(agents[0]
            .raw
            .get("body")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("gpt-5.4"));
    }

    #[test]
    fn agent_session_inheritance_defaults_to_true() {
        let (_td, root) = td_utf8();
        write(&root.join(".codex/agents/a/agent.toml"), "model = \"gpt\"");
        let settings = CodexSettings {
            codex_home: root.join("absent"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let agents = discover_agents(&settings, std::slice::from_ref(&root), &mut diags);
        assert!(agents[0].inherits_session);
    }
}
