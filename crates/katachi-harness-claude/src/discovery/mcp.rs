//! MCP server discovery from settings fragments.
//!
//! Claude supports MCP fragments in a handful of shapes:
//!
//! - `.claude/settings.json` → `mcpServers` / `mcp_servers` object
//! - `.claude/settings.local.json` → same
//! - standalone `.claude/mcp.json` files
//! - plugin-packaged manifests (handled by [`super::plugin`])
//!
//! We emit one `ClaudeItemKind::McpServer` item per server, keyed by
//! server name. Multiple scopes can define the same name; the catalog
//! rejects duplicates, so we surface a `claude.duplicate-mcp` diagnostic
//! rather than overwriting one definition with another.

use camino::Utf8Path;
use katachi_core::harness::{DiscoveredItem, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};
use serde_json::Value;

use crate::discovery::{push_warning, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::item::ClaudeItemKind;
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

pub fn scan_mcp(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        scan_settings_for_mcp(state, dir, dir.scope, &dir.settings_json())?;
        scan_settings_for_mcp(state, dir, ClaudeScope::Local, &dir.local_settings_json())?;
        scan_mcp_json_file(state, dir.scope, &dir.path.join("mcp.json"))?;
    }
    Ok(())
}

fn scan_settings_for_mcp(
    state: &mut ScanState,
    _dir: &ClaudeDir,
    scope: ClaudeScope,
    path: &Utf8Path,
) -> Result<(), ClaudeDiscoveryError> {
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_source) => return Ok(()), // hooks scanner already warned
    };
    let Some(servers) = value
        .get("mcpServers")
        .or_else(|| value.get("mcp_servers"))
        .and_then(|v| v.as_object())
    else {
        return Ok(());
    };
    for (name, config) in servers {
        emit_mcp_item(state, scope, path, name, config.clone());
    }
    Ok(())
}

fn scan_mcp_json_file(
    state: &mut ScanState,
    scope: ClaudeScope,
    path: &Utf8Path,
) -> Result<(), ClaudeDiscoveryError> {
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(source) => {
            push_warning(
                &mut state.catalog,
                "claude.mcp-parse-failed",
                format!("could not parse `{path}`: {source}"),
            );
            return Ok(());
        }
    };

    // Two shapes: flat object of servers, or `{"mcpServers": {...}}`.
    let servers = value
        .get("mcpServers")
        .or_else(|| value.get("mcp_servers"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(|| value.as_object().cloned().unwrap_or_default());
    for (name, config) in &servers {
        emit_mcp_item(state, scope, path, name, config.clone());
    }
    Ok(())
}

fn emit_mcp_item(
    state: &mut ScanState,
    scope: ClaudeScope,
    path: &Utf8Path,
    name: &str,
    config: Value,
) {
    let item = DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Claude,
            ClaudeItemKind::McpServer.as_str(),
            name.to_string(),
        ),
        display_name: name.to_string(),
        source: ItemSource {
            path: Some(path.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("settings-mcp".into()),
        },
        packaging: None,
        raw: serde_json::json!({
            "name": name,
            "config": config,
            "scope": scope.as_str(),
        }),
        capabilities: Vec::new(),
        constraints: Vec::new(),
    };
    if let Err(err) = state.catalog.insert_item(item) {
        push_warning(
            &mut state.catalog,
            "claude.duplicate-mcp",
            format!("ignoring duplicate MCP `{name}` in `{path}`: {err}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};
    use camino::Utf8PathBuf;
    use katachi_core::harness::RosterCatalog;
    use std::fs;
    use tempfile::TempDir;

    fn scan(roots: &DiscoveredRoots) -> RosterCatalog {
        let mut state = ScanState::new();
        scan_mcp(&mut state, roots).unwrap();
        state.finalize()
    }

    #[test]
    fn picks_up_mcp_servers_from_settings_json() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude/settings.json"),
            r#"{
                "mcpServers": {
                    "chrome-devtools": {"command": "cd", "args": []},
                    "sqlite": {"command": "sq"}
                }
            }"#,
        )
        .unwrap();
        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        let kinds: Vec<_> = catalog
            .iter_items()
            .filter(|(ir, _)| ir.kind == "mcp_server")
            .map(|(ir, _)| ir.id.clone())
            .collect();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.iter().any(|x| x == "chrome-devtools"));
    }

    #[test]
    fn reads_standalone_mcp_json() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude/mcp.json"),
            r#"{
                "mcpServers": {
                    "github-api": {"command": "gh-mcp"}
                }
            }"#,
        )
        .unwrap();
        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::User,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        assert!(catalog.iter_items().any(|(ir, _)| ir.id == "github-api"));
    }

    #[test]
    fn duplicate_mcp_across_scopes_yields_diagnostic() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join("project/.claude")).unwrap();
        fs::create_dir_all(root.join("user/.claude")).unwrap();
        fs::write(
            root.join("project/.claude/settings.json"),
            r#"{"mcpServers": {"dup": {}}}"#,
        )
        .unwrap();
        fs::write(
            root.join("user/.claude/settings.json"),
            r#"{"mcpServers": {"dup": {}}}"#,
        )
        .unwrap();
        let roots = DiscoveredRoots {
            claude_dirs: vec![
                ClaudeDir {
                    path: root.join("project/.claude"),
                    scope: ClaudeScope::Project,
                },
                ClaudeDir {
                    path: root.join("user/.claude"),
                    scope: ClaudeScope::User,
                },
            ],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.duplicate-mcp"));
    }
}
