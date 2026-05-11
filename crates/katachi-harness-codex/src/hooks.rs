//! Codex hook discovery.
//!
//! Codex supports `hooks.json` files that live near active config layers.
//! Critically, *multiple* matching hook files all fire — higher-precedence
//! layers do not simply replace hooks from lower-precedence layers. This
//! module surfaces every `hooks.json` it finds as its own [`HookSet`]
//! item, with a `HookAddsBehavior` edge tying each back to its parent
//! config layer. Downstream callers rely on the additive semantics when
//! materializing overlays.

use camino::Utf8PathBuf;
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};
use serde_json::Value as JsonValue;

use crate::config_layers::ConfigLayer;
use crate::items::{CodexEdgeKind, CodexItemKind};

/// One `hooks.json` instance.
#[derive(Clone, Debug)]
pub struct HookSet {
    pub id: String,
    pub path: Utf8PathBuf,
    pub layer_id: String,
    pub raw: JsonValue,
}

impl HookSet {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(
            HarnessKind::Codex,
            CodexItemKind::HookSet.as_str(),
            &self.id,
        )
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "layer": self.layer_id,
            "path": self.path.as_str(),
            "hooks": self.raw,
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: format!("hooks@{}", self.layer_id),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some("hooks".into()),
                provenance: Some(format!("codex.hooks:{}", self.layer_id)),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// Find every `hooks.json` adjacent to a discovered config layer.
pub fn discover_hooks(layers: &[ConfigLayer], diagnostics: &mut Vec<Diagnostic>) -> Vec<HookSet> {
    let mut out: Vec<HookSet> = Vec::new();
    for layer in layers {
        let Some(parent_dir) = layer.path.parent() else {
            continue;
        };
        let hooks_json = parent_dir.join("hooks.json");
        if !hooks_json.is_file() {
            continue;
        }
        let raw = match std::fs::read_to_string(&hooks_json) {
            Ok(s) => s,
            Err(err) => {
                diagnostics.push(Diagnostic::warning(
                    "codex.hooks.read",
                    format!("failed to read `{hooks_json}`: {err}"),
                ));
                continue;
            }
        };
        let parsed: JsonValue = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                diagnostics.push(Diagnostic::error(
                    "codex.hooks.parse",
                    format!("failed to parse `{hooks_json}`: {err}"),
                ));
                continue;
            }
        };
        out.push(HookSet {
            id: format!("{}:{}", layer.id, hooks_json),
            path: hooks_json,
            layer_id: layer.id.clone(),
            raw: parsed,
        });
    }
    out
}

/// Insert `HookAddsBehavior` edges from each hook set to its parent layer.
pub fn insert_hook_edges(
    hooks: &[HookSet],
    layers: &[ConfigLayer],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for hook in hooks {
        let Some(layer) = layers.iter().find(|l| l.id == hook.layer_id) else {
            continue;
        };
        let edge = DependencyEdge {
            from: hook.item_ref(),
            to: layer.item_ref(),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some(CodexEdgeKind::HookAddsBehavior.as_str().to_string()),
        };
        match catalog.insert_edge(edge) {
            Ok(()) | Err(RosterBuildError::DuplicateEdge { .. }) => {}
            Err(err) => diagnostics.push(Diagnostic::warning(
                "codex.hooks.edge",
                format!("failed to insert hook edge: {err}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_layers::{ConfigLayer, ConfigSource};
    use tempfile::TempDir;

    fn write(p: &camino::Utf8Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn discovers_hooks_next_to_layer() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let layer_path = root.join(".codex/config.toml");
        write(&layer_path, "a = 1");
        write(&root.join(".codex/hooks.json"), r#"{"before_tool":[]}"#);

        let layer = ConfigLayer {
            id: "project:/r".into(),
            source: ConfigSource::Project,
            path: layer_path,
            active: true,
            trust_required: true,
            raw: toml::Value::Integer(1),
            precedence: 200,
            profiles: Vec::new(),
        };

        let mut diags = Vec::new();
        let hooks = discover_hooks(&[layer], &mut diags);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].layer_id, "project:/r");
    }

    #[test]
    fn parse_error_surfaced() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let layer_path = root.join(".codex/config.toml");
        write(&layer_path, "a = 1");
        write(&root.join(".codex/hooks.json"), "not-json");
        let layer = ConfigLayer {
            id: "x".into(),
            source: ConfigSource::Project,
            path: layer_path,
            active: true,
            trust_required: true,
            raw: toml::Value::Integer(1),
            precedence: 200,
            profiles: Vec::new(),
        };
        let mut diags = Vec::new();
        let hooks = discover_hooks(&[layer], &mut diags);
        assert!(hooks.is_empty());
        assert!(diags.iter().any(|d| d.code == "codex.hooks.parse"));
    }
}
