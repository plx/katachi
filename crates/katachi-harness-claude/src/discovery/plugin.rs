//! Installed-plugin discovery.
//!
//! Each configured plugin root is treated as a directory of plugin
//! bundles. For every immediate subdirectory we:
//!
//! - parse `plugin.json` if present (looking for the plugin id + manifest
//!   pointers)
//! - fall back to the directory name as the plugin id when no manifest
//!   exists
//! - inspect conventional subdirectories (`skills/`, `agents/`, `hooks/`,
//!   `mcps/` or `mcp-servers/`, `output-styles/`) and emit a
//!   `ClaudeItemKind` item plus a `Contains` packaging edge for every
//!   file we recognize
//!
//! Plugin-packaged skills and agents reuse the loose-parser frontmatter
//! logic so the same fields behave consistently regardless of source.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::harness::{DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, PackageRef};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::config::ClaudeConfig;
use crate::discovery::{push_warning, PendingEdge, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::frontmatter;
use crate::item::{ClaudeEdgeLabel, ClaudeItemKind};
use crate::paths::{ClaudeScope, DiscoveredRoots, ScopedPath};

pub fn scan_plugins(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
    _config: &ClaudeConfig,
) -> Result<(), ClaudeDiscoveryError> {
    for root in roots.existing_plugin_roots() {
        scan_plugin_root(state, root)?;
    }
    Ok(())
}

fn scan_plugin_root(
    state: &mut ScanState,
    root: &ScopedPath,
) -> Result<(), ClaudeDiscoveryError> {
    let read = std::fs::read_dir(root.path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: root.path.clone(),
            source,
        }
    })?;
    let mut plugin_dirs: Vec<Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: root.path.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(utf8) = Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        let Ok(meta) = utf8.metadata() else { continue };
        if meta.is_dir() {
            plugin_dirs.push(utf8);
        }
    }
    plugin_dirs.sort();
    for dir in plugin_dirs {
        scan_plugin_dir(state, &dir, root.scope)?;
    }
    Ok(())
}

fn scan_plugin_dir(
    state: &mut ScanState,
    dir: &Utf8Path,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let manifest_path = dir.join("plugin.json");
    let manifest = if manifest_path.exists() {
        match parse_manifest(&manifest_path) {
            Ok(m) => Some(m),
            Err(err) => {
                push_warning(
                    &mut state.catalog,
                    "claude.plugin-manifest-parse",
                    format!("could not parse `{manifest_path}`: {err}"),
                );
                None
            }
        }
    } else {
        None
    };

    // Skip dirs that don't look like plugins: no manifest file at all
    // and no conventional subdirs. These are typically marketplace
    // scratch dirs (`cache/`, `repos/`, `.install-manifests/`) that
    // shouldn't pollute the roster. Dirs whose name starts with `.`
    // are always skipped. A manifest file that *exists* but failed to
    // parse still counts as a "this is a plugin dir" signal — the
    // user will see the parse diagnostic and can fix it.
    if dir
        .file_name()
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
    {
        return Ok(());
    }
    let has_conventional_subdir = [
        "skills",
        "agents",
        "hooks",
        "mcps",
        "mcp-servers",
        "mcp_servers",
        "output-styles",
        "commands",
    ]
    .iter()
    .any(|name| dir.join(name).exists());
    if !manifest_path.exists() && !has_conventional_subdir {
        return Ok(());
    }

    let id = manifest
        .as_ref()
        .and_then(|m| m.id.clone())
        .unwrap_or_else(|| dir.file_name().unwrap_or("plugin").to_string());
    let display_name = manifest
        .as_ref()
        .and_then(|m| m.display_name.clone())
        .unwrap_or_else(|| id.clone());
    let version = manifest.as_ref().and_then(|m| m.version.clone());

    let plugin_ref = ItemRef::new(
        HarnessKind::Claude,
        ClaudeItemKind::Plugin.as_str(),
        id.clone(),
    );
    let plugin_item = DiscoveredItem {
        item_ref: plugin_ref.clone(),
        display_name,
        source: ItemSource {
            path: Some(dir.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("plugin-dir".into()),
        },
        packaging: None,
        raw: serde_json::json!({
            "id": id,
            "version": version,
            "manifest_path": manifest_path.to_string(),
            "raw_manifest": manifest.as_ref().map(|m| m.raw.clone()),
        }),
        capabilities: Vec::new(),
        constraints: Vec::new(),
    };
    if let Err(err) = state.catalog.insert_item(plugin_item) {
        push_warning(
            &mut state.catalog,
            "claude.duplicate-plugin",
            format!("ignoring duplicate plugin `{id}` at `{dir}`: {err}"),
        );
        return Ok(());
    }

    // Packaged skills.
    let skills_dir = dir.join("skills");
    if skills_dir.exists() {
        scan_packaged_skills(state, &skills_dir, &plugin_ref, scope)?;
    }
    // Packaged agents.
    let agents_dir = dir.join("agents");
    if agents_dir.exists() {
        scan_packaged_agents(state, &agents_dir, &plugin_ref, scope)?;
    }
    // Packaged hooks (file names only here; hook scanner in Step 7 adds
    // settings-layer integration).
    let hooks_dir = dir.join("hooks");
    if hooks_dir.exists() {
        scan_packaged_hooks(state, &hooks_dir, &plugin_ref, scope)?;
    }
    // Packaged MCP manifests.
    for name in ["mcps", "mcp-servers", "mcp_servers"] {
        let mcp_dir = dir.join(name);
        if mcp_dir.exists() {
            scan_packaged_mcps(state, &mcp_dir, &plugin_ref, scope)?;
        }
    }
    // Packaged output styles.
    let styles_dir = dir.join("output-styles");
    if styles_dir.exists() {
        scan_packaged_output_styles(state, &styles_dir, &plugin_ref, scope)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct PluginManifest {
    id: Option<String>,
    display_name: Option<String>,
    version: Option<String>,
    raw: serde_json::Value,
}

fn parse_manifest(path: &Utf8Path) -> Result<PluginManifest, ClaudeDiscoveryError> {
    let raw_text = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let raw: serde_json::Value = serde_json::from_str(&raw_text).map_err(|source| {
        ClaudeDiscoveryError::Json {
            path: path.to_owned(),
            source,
        }
    })?;
    let id = raw.get("id").or_else(|| raw.get("name")).and_then(|v| v.as_str()).map(str::to_string);
    let display_name = raw
        .get("display_name")
        .or_else(|| raw.get("displayName"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let version = raw
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(PluginManifest {
        id,
        display_name,
        version,
        raw,
    })
}

fn scan_packaged_skills(
    state: &mut ScanState,
    skills_dir: &Utf8Path,
    plugin_ref: &ItemRef,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let entries = list_sorted_children(skills_dir)?;
    for entry in entries {
        let Ok(meta) = entry.metadata() else { continue };
        // Two layouts: skills/<name>/SKILL.md or skills/<name>.md.
        let skill_md = if meta.is_dir() {
            entry.join("SKILL.md")
        } else if meta.is_file() && entry.extension() == Some("md") {
            entry.clone()
        } else {
            continue;
        };
        if !skill_md.exists() {
            continue;
        }
        match parse_packaged_skill(&skill_md, scope, plugin_ref) {
            Ok((item, pending)) => {
                let item_ref = item.item_ref.clone();
                if let Err(err) = state.catalog.insert_item(item) {
                    push_warning(
                        &mut state.catalog,
                        "claude.duplicate-skill",
                        format!("ignoring duplicate skill at `{skill_md}`: {err}"),
                    );
                    continue;
                }
                emit_contains_edge(state, plugin_ref, &item_ref);
                for p in pending {
                    state.pending_edges.push(PendingEdge {
                        from: item_ref.clone(),
                        target_kind: p.target_kind,
                        target_id: p.target_id,
                        edge_kind: EdgeKind::Semantic,
                        label: p.label,
                        required: p.required,
                        missing_code: "claude.plugin-skill-missing-target".into(),
                    });
                }
            }
            Err(err) => {
                push_warning(
                    &mut state.catalog,
                    "claude.plugin-skill-parse-failed",
                    format!("could not parse plugin skill `{skill_md}`: {err}"),
                );
            }
        }
    }
    Ok(())
}

fn scan_packaged_agents(
    state: &mut ScanState,
    agents_dir: &Utf8Path,
    plugin_ref: &ItemRef,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let entries = list_sorted_children(agents_dir)?;
    for path in entries {
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() || path.extension() != Some("md") {
            continue;
        }
        match parse_packaged_agent(&path, scope, plugin_ref) {
            Ok((item, pending)) => {
                let item_ref = item.item_ref.clone();
                if let Err(err) = state.catalog.insert_item(item) {
                    push_warning(
                        &mut state.catalog,
                        "claude.duplicate-agent",
                        format!("ignoring duplicate agent at `{path}`: {err}"),
                    );
                    continue;
                }
                emit_contains_edge(state, plugin_ref, &item_ref);
                for p in pending {
                    state.pending_edges.push(PendingEdge {
                        from: item_ref.clone(),
                        target_kind: p.target_kind,
                        target_id: p.target_id,
                        edge_kind: EdgeKind::Semantic,
                        label: p.label,
                        required: p.required,
                        missing_code: "claude.plugin-agent-missing-target".into(),
                    });
                }
            }
            Err(err) => {
                push_warning(
                    &mut state.catalog,
                    "claude.plugin-agent-parse-failed",
                    format!("could not parse plugin agent `{path}`: {err}"),
                );
            }
        }
    }
    Ok(())
}

fn scan_packaged_hooks(
    state: &mut ScanState,
    hooks_dir: &Utf8Path,
    plugin_ref: &ItemRef,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let entries = list_sorted_children(hooks_dir)?;
    for path in entries {
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or("hook").to_string();
        let id = format!("{}:{}", plugin_ref.id, name);
        let item = DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Claude,
                ClaudeItemKind::HookSet.as_str(),
                id.clone(),
            ),
            display_name: name.clone(),
            source: ItemSource {
                path: Some(path.clone()),
                scope: Some(scope.as_str().into()),
                provenance: Some("plugin-hook".into()),
            },
            packaging: Some(PackageRef {
                item_ref: plugin_ref.clone(),
                required: true,
            }),
            raw: serde_json::json!({
                "name": name,
                "path": path.to_string(),
            }),
            capabilities: Vec::new(),
            constraints: Vec::new(),
        };
        if let Err(err) = state.catalog.insert_item(item.clone()) {
            push_warning(
                &mut state.catalog,
                "claude.duplicate-hook",
                format!("ignoring duplicate hook `{id}`: {err}"),
            );
            continue;
        }
        emit_contains_edge(state, plugin_ref, &item.item_ref);
    }
    Ok(())
}

fn scan_packaged_mcps(
    state: &mut ScanState,
    mcp_dir: &Utf8Path,
    plugin_ref: &ItemRef,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let entries = list_sorted_children(mcp_dir)?;
    for path in entries {
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() || path.extension() != Some("json") {
            continue;
        }
        let stem = path.file_stem().unwrap_or("").to_string();
        if stem.is_empty() {
            continue;
        }
        let raw_text =
            std::fs::read_to_string(path.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
                path: path.clone(),
                source,
            })?;
        let raw: serde_json::Value = match serde_json::from_str(&raw_text) {
            Ok(v) => v,
            Err(source) => {
                push_warning(
                    &mut state.catalog,
                    "claude.plugin-mcp-parse-failed",
                    format!("could not parse MCP manifest `{path}`: {source}"),
                );
                continue;
            }
        };
        let id = raw
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| stem.clone());
        let item = DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Claude,
                ClaudeItemKind::McpServer.as_str(),
                id.clone(),
            ),
            display_name: id.clone(),
            source: ItemSource {
                path: Some(path.clone()),
                scope: Some(scope.as_str().into()),
                provenance: Some("plugin-mcp".into()),
            },
            packaging: Some(PackageRef {
                item_ref: plugin_ref.clone(),
                required: true,
            }),
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        };
        let item_ref = item.item_ref.clone();
        if let Err(err) = state.catalog.insert_item(item) {
            push_warning(
                &mut state.catalog,
                "claude.duplicate-mcp",
                format!("ignoring duplicate MCP `{id}`: {err}"),
            );
            continue;
        }
        emit_contains_edge(state, plugin_ref, &item_ref);
    }
    Ok(())
}

fn scan_packaged_output_styles(
    state: &mut ScanState,
    dir: &Utf8Path,
    plugin_ref: &ItemRef,
    scope: ClaudeScope,
) -> Result<(), ClaudeDiscoveryError> {
    let entries = list_sorted_children(dir)?;
    for path in entries {
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let stem = path.file_stem().unwrap_or("style").to_string();
        let id = format!("{}:{stem}", plugin_ref.id);
        let item = DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Claude,
                ClaudeItemKind::OutputStyle.as_str(),
                id.clone(),
            ),
            display_name: stem.clone(),
            source: ItemSource {
                path: Some(path.clone()),
                scope: Some(scope.as_str().into()),
                provenance: Some("plugin-output-style".into()),
            },
            packaging: Some(PackageRef {
                item_ref: plugin_ref.clone(),
                required: true,
            }),
            raw: serde_json::json!({
                "stem": stem,
                "path": path.to_string(),
            }),
            capabilities: Vec::new(),
            constraints: Vec::new(),
        };
        let item_ref = item.item_ref.clone();
        if let Err(err) = state.catalog.insert_item(item) {
            push_warning(
                &mut state.catalog,
                "claude.duplicate-output-style",
                format!("ignoring duplicate output style `{id}`: {err}"),
            );
            continue;
        }
        emit_contains_edge(state, plugin_ref, &item_ref);
    }
    Ok(())
}

fn list_sorted_children(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, ClaudeDiscoveryError> {
    let read = std::fs::read_dir(dir.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut out: Vec<Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let path = entry.path();
        let Some(utf8) = Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        out.push(utf8);
    }
    out.sort();
    Ok(out)
}

fn emit_contains_edge(state: &mut ScanState, plugin_ref: &ItemRef, child: &ItemRef) {
    let edge = DependencyEdge {
        from: plugin_ref.clone(),
        to: child.clone(),
        kind: EdgeKind::Packaging,
        required: true,
        note: Some(ClaudeEdgeLabel::Contains.as_str().to_string()),
    };
    if let Err(err) = state.catalog.insert_edge(edge) {
        push_warning(
            &mut state.catalog,
            "claude.duplicate-edge",
            format!("ignoring duplicate contains edge: {err}"),
        );
    }
}

// Small shared struct so plugin skill/agent parsers can stash pending edges.
struct ParsedPending {
    target_kind: ClaudeItemKind,
    target_id: String,
    label: ClaudeEdgeLabel,
    required: bool,
}

fn parse_packaged_skill(
    path: &Utf8Path,
    scope: ClaudeScope,
    plugin_ref: &ItemRef,
) -> Result<(DiscoveredItem, Vec<ParsedPending>), ClaudeDiscoveryError> {
    let source = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let doc = frontmatter::parse(path, &source)?;
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .unwrap_or("")
        .to_string();
    let file_stem = path.file_stem().unwrap_or("").to_string();
    let fallback = if dir_name.is_empty() || dir_name == "skills" {
        file_stem
    } else {
        dir_name
    };
    let name = doc
        .get_str("name")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback);
    let description = doc.get_str("description").unwrap_or("").to_string();
    let agent = doc.get_str("agent").map(str::to_string);
    let allowed_tools: Vec<String> = doc
        .get_list("allowed-tools")
        .or_else(|| doc.get_list("allowed_tools"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mcp_refs: Vec<String> = doc
        .get_list("mcp_servers")
        .or_else(|| doc.get_list("mcp"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mcp_required = doc.get_bool("mcp_required").unwrap_or(false);

    let raw = serde_json::json!({
        "name": name,
        "description": description,
        "agent": agent,
        "mcp_servers": mcp_refs,
        "mcp_required": mcp_required,
        "allowed_tools": allowed_tools,
        "body": doc.body,
    });

    let item = DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Claude, ClaudeItemKind::Skill.as_str(), name.clone()),
        display_name: if description.is_empty() {
            name.clone()
        } else {
            format!("{name} — {description}")
        },
        source: ItemSource {
            path: Some(path.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("plugin-skill".into()),
        },
        packaging: Some(PackageRef {
            item_ref: plugin_ref.clone(),
            required: true,
        }),
        raw,
        capabilities: allowed_tools.iter().map(|t| format!("tool:{t}")).collect(),
        constraints: Vec::new(),
    };

    let mut pending = Vec::new();
    if let Some(agent_id) = agent {
        pending.push(ParsedPending {
            target_kind: ClaudeItemKind::Agent,
            target_id: agent_id,
            label: ClaudeEdgeLabel::SkillUsesAgent,
            required: false,
        });
    }
    for mcp in mcp_refs {
        pending.push(ParsedPending {
            target_kind: ClaudeItemKind::McpServer,
            target_id: mcp,
            label: if mcp_required {
                ClaudeEdgeLabel::ItemRequiresMcp
            } else {
                ClaudeEdgeLabel::ItemSuggestsMcp
            },
            required: mcp_required,
        });
    }
    Ok((item, pending))
}

fn parse_packaged_agent(
    path: &Utf8Path,
    scope: ClaudeScope,
    plugin_ref: &ItemRef,
) -> Result<(DiscoveredItem, Vec<ParsedPending>), ClaudeDiscoveryError> {
    let source = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let doc = frontmatter::parse(path, &source)?;
    let stem = path.file_stem().unwrap_or("").to_string();
    let name = doc
        .get_str("name")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let description = doc.get_str("description").unwrap_or("").to_string();
    let model = doc.get_str("model").map(str::to_string);
    let preloaded_skills: Vec<String> = doc
        .get_list("preloaded_skills")
        .or_else(|| doc.get_list("skills"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mcp_refs: Vec<String> = doc
        .get_list("mcp_servers")
        .or_else(|| doc.get_list("mcp"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mcp_required = doc.get_bool("mcp_required").unwrap_or(false);
    let tools: Vec<String> = doc
        .get_list("tools")
        .or_else(|| doc.get_list("allowed-tools"))
        .or_else(|| doc.get_list("allowed_tools"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();

    let raw = serde_json::json!({
        "name": name,
        "description": description,
        "model": model,
        "tools": tools,
        "preloaded_skills": preloaded_skills,
        "mcp_servers": mcp_refs,
        "mcp_required": mcp_required,
        "body": doc.body,
    });

    let mut capabilities = Vec::new();
    for t in &tools {
        capabilities.push(format!("tool:{t}"));
    }
    if let Some(m) = &model {
        capabilities.push(format!("model:{m}"));
    }

    let item = DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Claude, ClaudeItemKind::Agent.as_str(), name.clone()),
        display_name: if description.is_empty() {
            name.clone()
        } else {
            format!("{name} — {description}")
        },
        source: ItemSource {
            path: Some(path.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("plugin-agent".into()),
        },
        packaging: Some(PackageRef {
            item_ref: plugin_ref.clone(),
            required: true,
        }),
        raw,
        capabilities,
        constraints: Vec::new(),
    };

    let mut pending = Vec::new();
    for skill in preloaded_skills {
        pending.push(ParsedPending {
            target_kind: ClaudeItemKind::Skill,
            target_id: skill,
            label: ClaudeEdgeLabel::AgentPreloadsSkill,
            required: false,
        });
    }
    for mcp in mcp_refs {
        pending.push(ParsedPending {
            target_kind: ClaudeItemKind::McpServer,
            target_id: mcp,
            label: if mcp_required {
                ClaudeEdgeLabel::ItemRequiresMcp
            } else {
                ClaudeEdgeLabel::ItemSuggestsMcp
            },
            required: mcp_required,
        });
    }
    Ok((item, pending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{DiscoveredRoots, ScopedPath};
    use camino::Utf8PathBuf;
    use katachi_core::harness::RosterCatalog;
    use std::fs;
    use tempfile::TempDir;

    fn scan_with_plugin_root(root: Utf8PathBuf, scope: ClaudeScope) -> RosterCatalog {
        let mut state = ScanState::new();
        let roots = DiscoveredRoots {
            claude_dirs: Vec::new(),
            top_level_claude_mds: Vec::new(),
            plugin_roots: vec![ScopedPath { path: root, scope }],
        };
        let config = ClaudeConfig::default();
        scan_plugins(&mut state, &roots, &config).unwrap();
        state.finalize()
    }

    #[test]
    fn plugin_with_manifest_and_skill_produces_contains_edge() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("web-a11y");
        fs::create_dir_all(plugin.join("skills/axe-runner")).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"id": "web-a11y", "display_name": "Web A11y", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("skills/axe-runner/SKILL.md"),
            "---\nname: axe-runner\n---\nbody",
        )
        .unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        let kinds: Vec<&str> = catalog
            .iter_items()
            .map(|(ir, _)| ir.kind.as_str())
            .collect();
        assert!(kinds.contains(&"plugin"));
        assert!(kinds.contains(&"skill"));
        let contains_edges = catalog
            .iter_edges()
            .filter(|e| e.note.as_deref() == Some("contains"))
            .count();
        assert_eq!(contains_edges, 1);
    }

    #[test]
    fn plugin_with_no_manifest_uses_directory_name() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("manifest-less-plugin");
        // Conventional subdir so the scanner recognizes this as a plugin
        // rather than a scratch dir like `cache/` or `repos/`.
        fs::create_dir_all(plugin.join("skills")).unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginProject);
        assert_eq!(catalog.items.len(), 1);
        let ir = catalog.items.keys().next().unwrap();
        assert_eq!(ir.id, "manifest-less-plugin");
    }

    #[test]
    fn dirs_without_manifest_or_conventional_subdirs_are_skipped() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::create_dir_all(root.join("repos")).unwrap();
        fs::create_dir_all(root.join(".install-manifests")).unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        assert!(catalog.items.is_empty());
    }

    #[test]
    fn packaged_skill_gets_packaging_ref() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("p1");
        fs::create_dir_all(plugin.join("skills/greet")).unwrap();
        fs::write(plugin.join("plugin.json"), r#"{"id": "p1"}"#).unwrap();
        fs::write(
            plugin.join("skills/greet/SKILL.md"),
            "---\nname: greet\n---\n",
        )
        .unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        let skill = catalog
            .iter_items()
            .find(|(ir, _)| ir.kind == "skill")
            .unwrap()
            .1;
        assert_eq!(
            skill.packaging.as_ref().unwrap().item_ref.id,
            "p1"
        );
        assert!(skill.packaging.as_ref().unwrap().required);
    }

    #[test]
    fn plugin_scans_agents_hooks_mcps_and_output_styles() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("full");
        fs::create_dir_all(plugin.join("agents")).unwrap();
        fs::create_dir_all(plugin.join("hooks")).unwrap();
        fs::create_dir_all(plugin.join("mcps")).unwrap();
        fs::create_dir_all(plugin.join("output-styles")).unwrap();
        fs::write(plugin.join("plugin.json"), r#"{"id": "full"}"#).unwrap();
        fs::write(
            plugin.join("agents/reviewer.md"),
            "---\nname: reviewer\n---\n",
        )
        .unwrap();
        fs::write(plugin.join("hooks/pre-commit.sh"), "#!/bin/sh").unwrap();
        fs::write(
            plugin.join("mcps/chrome.json"),
            r#"{"name": "chrome-dev"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("output-styles/minimal.md"),
            "minimal style",
        )
        .unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        let kinds: std::collections::BTreeSet<_> = catalog
            .iter_items()
            .map(|(ir, _)| ir.kind.clone())
            .collect();
        assert!(kinds.contains("agent"));
        assert!(kinds.contains("hook_set"));
        assert!(kinds.contains("mcp_server"));
        assert!(kinds.contains("output_style"));

        // Plugin contains edge count = 4 (agent, hook, mcp, style).
        let contains = catalog
            .iter_edges()
            .filter(|e| e.note.as_deref() == Some("contains"))
            .count();
        assert_eq!(contains, 4);
    }

    #[test]
    fn plugin_skill_with_agent_connects_after_finalize() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("p1");
        fs::create_dir_all(plugin.join("skills/axe")).unwrap();
        fs::create_dir_all(plugin.join("agents")).unwrap();
        fs::write(plugin.join("plugin.json"), r#"{"id": "p1"}"#).unwrap();
        fs::write(
            plugin.join("skills/axe/SKILL.md"),
            "---\nname: axe\nagent: reviewer\n---\n",
        )
        .unwrap();
        fs::write(
            plugin.join("agents/reviewer.md"),
            "---\nname: reviewer\n---\n",
        )
        .unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        let notes: std::collections::BTreeSet<_> = catalog
            .iter_edges()
            .filter_map(|e| e.note.clone())
            .collect();
        assert!(notes.contains("skill_uses_agent"));
        assert!(notes.contains("contains"));
    }

    #[test]
    fn malformed_manifest_becomes_diagnostic_not_error() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plugin = root.join("bad");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("plugin.json"), "not { valid json").unwrap();

        let catalog = scan_with_plugin_root(root, ClaudeScope::PluginUser);
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.plugin-manifest-parse"));
        // Plugin item is still emitted using the directory name, since
        // a (malformed) manifest is a strong signal this is a plugin
        // dir even if we can't parse it.
        assert!(catalog.iter_items().any(|(ir, _)| ir.id == "bad"));
    }
}
