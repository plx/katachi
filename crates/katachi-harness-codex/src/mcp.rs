//! MCP server discovery from active Codex config layers.
//!
//! Codex declares MCP servers inside `config.toml` under
//! `[mcp_servers.<name>]` blocks. We extract those declarations into typed
//! `McpServer` items, preserving transport (stdio/http/streamable-http),
//! command/args, env, and any auth hints in the raw JSON payload.

use camino::Utf8PathBuf;
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource};

use crate::config_layers::ConfigLayer;
use crate::items::CodexItemKind;

#[derive(Clone, Debug)]
pub struct McpServer {
    pub id: String,
    /// The server's name as declared in `[mcp_servers.<name>]`.
    pub name: String,
    pub source_layer: String,
    pub transport: String,
    pub raw: serde_json::Value,
    pub path: Utf8PathBuf,
}

impl McpServer {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(HarnessKind::Codex, CodexItemKind::McpServer.as_str(), &self.id)
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "name": self.name,
            "layer": self.source_layer,
            "transport": self.transport,
            "path": self.path.as_str(),
            "config": self.raw,
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.name.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(self.source_layer.clone()),
                provenance: Some("codex.mcp_server".into()),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

pub fn discover_mcp_servers(
    layers: &[ConfigLayer],
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<McpServer> {
    let mut out: Vec<McpServer> = Vec::new();
    for layer in layers {
        let Some(table) = layer.as_table() else {
            continue;
        };
        let Some(mcps) = table.get("mcp_servers").and_then(|v| v.as_table()) else {
            continue;
        };
        for (name, body) in mcps {
            let id = format!("{}:{}", layer.id, name);
            let transport = detect_transport(body);
            out.push(McpServer {
                id,
                name: name.clone(),
                source_layer: layer.id.clone(),
                transport,
                raw: toml_to_json(body),
                path: layer.path.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn detect_transport(value: &toml::Value) -> String {
    let table = match value.as_table() {
        Some(t) => t,
        None => return "unknown".into(),
    };
    if let Some(t) = table.get("transport").and_then(|v| v.as_str()) {
        return t.to_string();
    }
    if let Some(t) = table.get("type").and_then(|v| v.as_str()) {
        return t.to_string();
    }
    if table.contains_key("url") || table.contains_key("http_url") {
        return "http".into();
    }
    if table.contains_key("command") {
        return "stdio".into();
    }
    "unknown".into()
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
    use crate::config_layers::{ConfigLayer, ConfigSource};
    use camino::Utf8PathBuf;

    fn layer_with_mcp(body: &str) -> ConfigLayer {
        let raw: toml::Value = toml::from_str(body).unwrap();
        ConfigLayer {
            id: "user".into(),
            source: ConfigSource::User,
            path: Utf8PathBuf::from("/fake/config.toml"),
            active: true,
            trust_required: false,
            raw,
            precedence: 100,
            profiles: Vec::new(),
        }
    }

    #[test]
    fn extracts_mcp_blocks() {
        let layer = layer_with_mcp(
            r#"
[mcp_servers.chrome]
command = "chrome-mcp"
args = ["--port", "9229"]

[mcp_servers.openaiDeveloperDocs]
url = "https://example/openai-docs"
transport = "streamable-http"
"#,
        );
        let mut diags = Vec::new();
        let out = discover_mcp_servers(&[layer], &mut diags);
        let names: Vec<_> = out.iter().map(|m| m.name.clone()).collect();
        assert!(names.contains(&"chrome".to_string()));
        assert!(names.contains(&"openaiDeveloperDocs".to_string()));

        let chrome = out.iter().find(|m| m.name == "chrome").unwrap();
        assert_eq!(chrome.transport, "stdio");
        let docs = out.iter().find(|m| m.name == "openaiDeveloperDocs").unwrap();
        assert_eq!(docs.transport, "streamable-http");
    }

    #[test]
    fn no_mcp_blocks_returns_empty() {
        let layer = layer_with_mcp("model = \"gpt-5.4\"");
        let mut diags = Vec::new();
        let out = discover_mcp_servers(&[layer], &mut diags);
        assert!(out.is_empty());
    }
}
