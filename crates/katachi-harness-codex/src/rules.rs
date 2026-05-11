//! Codex rule-file discovery.
//!
//! Rules live in a rules directory adjacent to a config layer. Each rule
//! file is surfaced as a single `RuleSet` item that records its source
//! layer, raw body, and whether it is local/user/admin-enforced.

use camino::Utf8PathBuf;
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};

use crate::config_layers::{ConfigLayer, ConfigSource};
use crate::items::{CodexEdgeKind, CodexItemKind};

/// Rule enforcement tier: who set the rule.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RuleTier {
    Admin,
    User,
    Local,
}

impl RuleTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
            Self::Local => "local",
        }
    }
}

/// A rule file inside a `rules/` directory.
#[derive(Clone, Debug)]
pub struct RuleSet {
    pub id: String,
    pub path: Utf8PathBuf,
    pub layer_id: String,
    pub tier: RuleTier,
    pub body: String,
}

impl RuleSet {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(
            HarnessKind::Codex,
            CodexItemKind::RuleSet.as_str(),
            &self.id,
        )
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "layer": self.layer_id,
            "tier": self.tier.as_str(),
            "path": self.path.as_str(),
            "body": self.body,
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: format!("rules@{}", self.id),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(self.tier.as_str().to_string()),
                provenance: Some(format!("codex.rules:{}", self.layer_id)),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// Find every rule file beside each config layer's `rules/` directory.
pub fn discover_rules(layers: &[ConfigLayer], diagnostics: &mut Vec<Diagnostic>) -> Vec<RuleSet> {
    let mut out: Vec<RuleSet> = Vec::new();
    for layer in layers {
        let Some(parent) = layer.path.parent() else {
            continue;
        };
        let rules_dir = parent.join("rules");
        if !rules_dir.is_dir() {
            continue;
        }
        let tier = rule_tier_for_layer(layer);
        let read = match std::fs::read_dir(&rules_dir) {
            Ok(r) => r,
            Err(err) => {
                diagnostics.push(Diagnostic::warning(
                    "codex.rules.read-dir",
                    format!("failed to read rules dir `{rules_dir}`: {err}"),
                ));
                continue;
            }
        };
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(path) = Utf8PathBuf::from_path_buf(path) else {
                continue;
            };
            let body = match std::fs::read_to_string(&path) {
                Ok(b) => b,
                Err(err) => {
                    diagnostics.push(Diagnostic::warning(
                        "codex.rules.read",
                        format!("failed to read `{path}`: {err}"),
                    ));
                    continue;
                }
            };
            let name = path.file_name().unwrap_or("rule").to_string();
            out.push(RuleSet {
                id: format!("{}:{}", layer.id, name),
                path,
                layer_id: layer.id.clone(),
                tier,
                body,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn rule_tier_for_layer(layer: &ConfigLayer) -> RuleTier {
    match layer.source {
        ConfigSource::System => RuleTier::Admin,
        ConfigSource::User => RuleTier::User,
        ConfigSource::Project => RuleTier::Local,
    }
}

/// Insert `RuleConstrainsExecution` edges from each rule to its parent layer.
pub fn insert_rule_edges(
    rules: &[RuleSet],
    layers: &[ConfigLayer],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for rule in rules {
        let Some(layer) = layers.iter().find(|l| l.id == rule.layer_id) else {
            continue;
        };
        let edge = DependencyEdge {
            from: rule.item_ref(),
            to: layer.item_ref(),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some(CodexEdgeKind::RuleConstrainsExecution.as_str().to_string()),
        };
        match catalog.insert_edge(edge) {
            Ok(()) | Err(RosterBuildError::DuplicateEdge { .. }) => {}
            Err(err) => diagnostics.push(Diagnostic::warning(
                "codex.rules.edge",
                format!("failed to insert rule edge: {err}"),
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
    fn discovers_rules_for_each_layer() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let layer_path = root.join(".codex/config.toml");
        write(&layer_path, "a = 1");
        write(&root.join(".codex/rules/readonly.toml"), "allow = []");
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
        let rules = discover_rules(&[layer], &mut diags);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tier, RuleTier::Local);
    }
}
