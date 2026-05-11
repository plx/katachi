//! MCP server discovery.
//!
//! MCP servers appear in three places for Gemini:
//! 1. `settings.json` `"mcpServers"` block
//! 2. extension manifest `mcpServers`
//! 3. generated temporary overlays
//!
//! Settings-layer precedence is important: a settings-defined server
//! with the same name as an extension-provided server always wins.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use camino::{Utf8Path, Utf8PathBuf};

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::extension::ExtensionManifest;
use crate::item::GeminiItemKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub owner: McpOwner,
    pub config: Value,
    pub source_path: Option<Utf8PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpOwner {
    Extension { extension: String },
    Settings { scope: String },
    Generated,
}

impl McpOwner {
    pub fn as_scope(&self) -> &'static str {
        match self {
            Self::Extension { .. } => "extension",
            Self::Settings { .. } => "settings",
            Self::Generated => "generated",
        }
    }

    pub fn precedence(&self) -> u32 {
        // Higher number = higher precedence. Generated overlay tops
        // everything; settings beat extension; within settings, project
        // beats user (mirrors settings layer ranks).
        match self {
            Self::Generated => 100,
            Self::Settings { scope } => match scope.as_str() {
                "project" => 20,
                "user" => 10,
                _ => 5,
            },
            Self::Extension { .. } => 1,
        }
    }
}

/// Parse extension-provided MCP servers from a manifest.
pub fn parse_extension_manifest_servers(manifest: &ExtensionManifest) -> Option<Vec<McpServer>> {
    parse_servers(
        &manifest.mcp_servers_raw,
        McpOwner::Extension {
            extension: manifest.name.clone(),
        },
        None,
    )
}

/// Parse MCP servers from a settings layer body.
pub fn parse_settings_servers(
    body: &Value,
    settings_scope: &str,
    source_path: &Utf8Path,
) -> Option<Vec<McpServer>> {
    let raw = body.get("mcpServers")?;
    parse_servers(
        raw,
        McpOwner::Settings {
            scope: settings_scope.to_owned(),
        },
        Some(source_path.to_owned()),
    )
}

fn parse_servers(
    raw: &Value,
    owner: McpOwner,
    source_path: Option<Utf8PathBuf>,
) -> Option<Vec<McpServer>> {
    let map = raw.as_object()?;
    let mut servers: Vec<McpServer> = map
        .iter()
        .map(|(k, v)| McpServer {
            name: k.clone(),
            owner: owner.clone(),
            config: v.clone(),
            source_path: source_path.clone(),
        })
        .collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Some(servers)
}

pub fn to_discovered_item(server: &McpServer) -> DiscoveredItem {
    let packaging = match &server.owner {
        McpOwner::Extension { extension } => Some(PackageRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::Extension.as_str(),
                extension.clone(),
            ),
            required: true,
        }),
        _ => None,
    };
    let id = item_id(server);
    DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Gemini, GeminiItemKind::McpServer.as_str(), id),
        display_name: server.name.clone(),
        source: ItemSource {
            path: server.source_path.clone(),
            scope: Some(server.owner.as_scope().to_owned()),
            provenance: Some("mcp".into()),
        },
        packaging,
        raw: serde_json::json!({
            "name": server.name,
            "owner": server.owner,
            "config": server.config,
        }),
        capabilities: vec!["mcp-server".into()],
        constraints: Vec::new(),
    }
}

/// Compute a stable item id for an MCP server. Includes scope so two
/// same-named servers coming from different layers don't collide.
pub fn item_id(server: &McpServer) -> String {
    match &server.owner {
        McpOwner::Extension { extension } => format!("ext:{extension}:{}", server.name),
        McpOwner::Settings { scope } => format!("settings:{scope}:{}", server.name),
        McpOwner::Generated => format!("generated:{}", server.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn parse_extension_manifest_servers_finds_entries() {
        let manifest = ExtensionManifest::from_json(serde_json::json!({
            "name": "ext",
            "mcpServers": {"chrome": {"command": "node"}}
        }))
        .unwrap();
        let servers = parse_extension_manifest_servers(&manifest).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "chrome");
    }

    #[test]
    fn parse_settings_returns_none_when_absent() {
        let body = serde_json::json!({});
        assert!(parse_settings_servers(&body, "user", Utf8Path::new("/x")).is_none());
    }

    #[test]
    fn precedence_ordering() {
        let generated = McpOwner::Generated.precedence();
        let project = McpOwner::Settings {
            scope: "project".into(),
        }
        .precedence();
        let user = McpOwner::Settings {
            scope: "user".into(),
        }
        .precedence();
        let ext = McpOwner::Extension {
            extension: "e".into(),
        }
        .precedence();
        assert!(generated > project);
        assert!(project > user);
        assert!(user > ext);
    }

    #[test]
    fn item_id_is_scoped() {
        let s = McpServer {
            name: "chrome".into(),
            owner: McpOwner::Extension {
                extension: "pkg".into(),
            },
            config: Value::Null,
            source_path: None,
        };
        assert_eq!(item_id(&s), "ext:pkg:chrome");
    }

    #[test]
    fn item_has_scope_and_packaging_for_extension() {
        let s = McpServer {
            name: "chrome".into(),
            owner: McpOwner::Extension {
                extension: "pkg".into(),
            },
            config: serde_json::json!({"command": "node"}),
            source_path: Some(Utf8PathBuf::from("/x")),
        };
        let item = to_discovered_item(&s);
        assert!(item.packaging.is_some());
        assert_eq!(item.source.scope.as_deref(), Some("extension"));
    }
}
