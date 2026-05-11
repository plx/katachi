//! Codex plugin discovery.
//!
//! Codex plugins live in user- and project-scoped marketplace directories
//! (e.g. `~/.agents/plugins/`, `.agents/plugins/`). Each plugin directory
//! contains a `plugin.toml` manifest and optional bundled skills, agents,
//! hooks, and MCP server definitions. We emit one [`Plugin`] item per
//! discovered plugin plus "packaged" items for bundled capabilities, and
//! record packaging edges so the shared resolver's packaging closure
//! picks them up.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, PackageRef};

use crate::items::{CodexEdgeKind, CodexItemKind};
use crate::CodexSettings;

/// Metadata about a single discovered plugin.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub id: String,
    pub path: Utf8PathBuf,
    pub manifest_path: Utf8PathBuf,
    pub display_name: String,
    pub description: Option<String>,
    pub raw: serde_json::Value,
    /// Items the plugin packages: bundled skills, agents, hooks, mcps.
    pub packaged: Vec<PackagedCapability>,
}

#[derive(Clone, Debug)]
pub struct PackagedCapability {
    pub kind: CodexItemKind,
    pub id: String,
    pub display_name: String,
    pub path: Utf8PathBuf,
    pub raw: serde_json::Value,
}

impl Plugin {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(HarnessKind::Codex, CodexItemKind::Plugin.as_str(), &self.id)
    }

    pub fn to_item(&self) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.display_name.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some("plugin".into()),
                provenance: Some("codex.plugin".into()),
            },
            packaging: None,
            raw: self.raw.clone(),
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    /// Return the bundled items and their packaging edges.
    pub fn packaged_items(&self) -> Vec<(DiscoveredItem, DependencyEdge)> {
        self.packaged
            .iter()
            .map(|cap| {
                let item_ref = ItemRef::new(HarnessKind::Codex, cap.kind.as_str(), &cap.id);
                let item = DiscoveredItem {
                    item_ref: item_ref.clone(),
                    display_name: cap.display_name.clone(),
                    source: ItemSource {
                        path: Some(cap.path.clone()),
                        scope: Some("plugin-packaged".into()),
                        provenance: Some(format!("codex.plugin:{}", self.id)),
                    },
                    packaging: Some(PackageRef {
                        item_ref: self.item_ref(),
                        required: true,
                    }),
                    raw: cap.raw.clone(),
                    capabilities: Vec::new(),
                    constraints: Vec::new(),
                };
                let edge = DependencyEdge {
                    from: self.item_ref(),
                    to: item_ref,
                    kind: EdgeKind::Packaging,
                    required: true,
                    note: Some(CodexEdgeKind::PluginContains.as_str().to_string()),
                };
                (item, edge)
            })
            .collect()
    }
}

pub fn discover_plugins(
    settings: &CodexSettings,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Plugin> {
    let mut out: Vec<Plugin> = Vec::new();
    for root in &settings.marketplace_roots {
        if !root.is_dir() {
            continue;
        }
        let Ok(read) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(plugin_dir) = Utf8PathBuf::from_path_buf(path) else {
                continue;
            };
            if let Some(plugin) = load_plugin(&plugin_dir, diagnostics) {
                out.push(plugin);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn load_plugin(dir: &Utf8Path, diagnostics: &mut Vec<Diagnostic>) -> Option<Plugin> {
    let manifest_path = dir.join("plugin.toml");
    if !manifest_path.is_file() {
        return None;
    }
    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.plugin.read",
                format!("failed to read `{manifest_path}`: {err}"),
            ));
            return None;
        }
    };
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            diagnostics.push(Diagnostic::error(
                "codex.plugin.parse",
                format!("failed to parse `{manifest_path}`: {err}"),
            ));
            return None;
        }
    };
    let json = toml_to_json(&value);
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| dir.file_name().unwrap_or("plugin").to_string());
    let display_name = json
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());
    let description = json
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Packaged skills directory.
    let mut packaged: Vec<PackagedCapability> = Vec::new();
    let skills_dir = dir.join("skills");
    if skills_dir.is_dir() {
        if let Ok(read) = std::fs::read_dir(&skills_dir) {
            for entry in read.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let path = Utf8PathBuf::from_path_buf(entry.path()).ok();
                    if let Some(p) = path {
                        packaged.push(PackagedCapability {
                            kind: CodexItemKind::Skill,
                            id: format!("{}:{}", id, name),
                            display_name: name.clone(),
                            path: p,
                            raw: serde_json::json!({ "packaged_by": id.clone() }),
                        });
                    }
                }
            }
        }
    }

    // Packaged agents directory.
    let agents_dir = dir.join("agents");
    if agents_dir.is_dir() {
        if let Ok(read) = std::fs::read_dir(&agents_dir) {
            for entry in read.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let path = Utf8PathBuf::from_path_buf(entry.path()).ok();
                    if let Some(p) = path {
                        packaged.push(PackagedCapability {
                            kind: CodexItemKind::CustomAgent,
                            id: format!("{}:{}", id, name),
                            display_name: name.clone(),
                            path: p,
                            raw: serde_json::json!({ "packaged_by": id.clone() }),
                        });
                    }
                }
            }
        }
    }

    Some(Plugin {
        id: id.clone(),
        path: dir.to_path_buf(),
        manifest_path,
        display_name,
        description,
        raw: json,
        packaged,
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

    #[test]
    fn discovers_plugin_and_packaged_items() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let marketplace = root.join("market");
        let plugin_dir = marketplace.join("accessibility");
        write(
            &plugin_dir.join("plugin.toml"),
            r#"
id = "accessibility"
name = "Accessibility"
description = "A11y helpers"
"#,
        );
        write(&plugin_dir.join("skills/axe/SKILL.md"), "skill body");
        write(
            &plugin_dir.join("agents/reviewer/agent.toml"),
            "model = \"gpt\"",
        );

        let settings = CodexSettings {
            marketplace_roots: vec![marketplace],
            codex_home: root.join("absent"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let plugins = discover_plugins(&settings, &mut diags);
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!(p.id, "accessibility");
        assert_eq!(p.packaged.len(), 2);

        let packaged_kinds: Vec<_> = p.packaged.iter().map(|c| c.kind).collect();
        assert!(packaged_kinds.contains(&CodexItemKind::Skill));
        assert!(packaged_kinds.contains(&CodexItemKind::CustomAgent));
    }

    #[test]
    fn packaged_items_carry_edges() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let marketplace = root.join("market");
        let plugin_dir = marketplace.join("pk");
        write(&plugin_dir.join("plugin.toml"), r#"id = "pk""#);
        write(&plugin_dir.join("skills/alpha/SKILL.md"), "x");

        let settings = CodexSettings {
            marketplace_roots: vec![marketplace],
            codex_home: root.join("absent"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let plugins = discover_plugins(&settings, &mut diags);
        let p = &plugins[0];
        let items = p.packaged_items();
        assert_eq!(items.len(), 1);
        let (item, edge) = &items[0];
        assert!(item.packaging.is_some());
        assert_eq!(edge.kind, EdgeKind::Packaging);
        assert_eq!(edge.note.as_deref(), Some("plugin_contains"));
    }
}
