//! Loose agent discovery under `<root>/agents/*.md`.
//!
//! Each `.md` file under a scoped `.claude/agents/` is parsed into a
//! `ClaudeItemKind::Agent`. Frontmatter fields the Claude ecosystem uses
//! today are surfaced as typed capabilities/raw metadata:
//!
//! - `name`: item id (falls back to the file stem)
//! - `description`: human-readable summary
//! - `model`, `effort`: run-profile-ish fields (kept on the raw blob)
//! - `tools` / `allowed-tools`: capability list
//! - `preloaded_skills` / `skills`: emits `agent_preloads_skill` edges
//! - `mcp_servers` + `mcp_required`: emits `item_suggests_mcp` or
//!   `item_requires_mcp` edges
//! - `isolation`: kept on the raw blob for later SDK projection work

use camino::Utf8Path;
use katachi_core::harness::{DiscoveredItem, EdgeKind, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::discovery::{push_warning, PendingEdge, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::frontmatter;
use crate::item::{ClaudeEdgeLabel, ClaudeItemKind};
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

pub fn scan_loose_agents(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        discover_in_dir(state, dir)?;
    }
    Ok(())
}

fn discover_in_dir(state: &mut ScanState, dir: &ClaudeDir) -> Result<(), ClaudeDiscoveryError> {
    let agents_dir = dir.agents_dir();
    if !agents_dir.exists() {
        return Ok(());
    }
    let read =
        std::fs::read_dir(agents_dir.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
            path: agents_dir.clone(),
            source,
        })?;
    let mut files: Vec<camino::Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: agents_dir.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(utf8) = camino::Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        if utf8.extension() != Some("md") {
            continue;
        }
        if !utf8.is_file() {
            continue;
        }
        files.push(utf8);
    }
    files.sort();

    for file in files {
        match parse_agent_file(&file, dir.scope) {
            Ok(Some((item, pending))) => {
                let item_ref = item.item_ref.clone();
                if let Err(err) = state.catalog.insert_item(item) {
                    push_warning(
                        &mut state.catalog,
                        "claude.duplicate-agent",
                        format!("ignoring duplicate agent at `{file}`: {err}"),
                    );
                    continue;
                }
                for p in pending {
                    state.pending_edges.push(p.into_pending(&item_ref));
                }
            }
            Ok(None) => {
                push_warning(
                    &mut state.catalog,
                    "claude.agent-missing-name",
                    format!("agent at `{file}` has no usable name; skipped"),
                );
            }
            Err(err) => {
                push_warning(
                    &mut state.catalog,
                    "claude.agent-parse-failed",
                    format!("could not parse agent at `{file}`: {err}"),
                );
            }
        }
    }
    Ok(())
}

struct AgentPending {
    label: ClaudeEdgeLabel,
    target_kind: ClaudeItemKind,
    target_id: String,
    required: bool,
}

impl AgentPending {
    fn into_pending(self, from: &ItemRef) -> PendingEdge {
        PendingEdge {
            from: from.clone(),
            target_kind: self.target_kind,
            target_id: self.target_id,
            edge_kind: EdgeKind::Semantic,
            label: self.label,
            required: self.required,
            missing_code: "claude.agent-missing-target".into(),
        }
    }
}

fn parse_agent_file(
    path: &Utf8Path,
    scope: ClaudeScope,
) -> Result<Option<(DiscoveredItem, Vec<AgentPending>)>, ClaudeDiscoveryError> {
    let source =
        std::fs::read_to_string(path.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        })?;
    let doc = frontmatter::parse(path, &source)?;

    let stem = path
        .file_stem()
        .ok_or_else(|| ClaudeDiscoveryError::NonUtf8 {
            path: path.to_owned(),
        })?;

    let name = doc
        .get_str("name")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stem.to_string());
    if name.is_empty() {
        return Ok(None);
    }

    let description = doc.get_str("description").unwrap_or("").to_string();
    let model = doc.get_str("model").map(str::to_string);
    let effort = doc.get_str("effort").map(str::to_string);
    let isolation = doc.get_str("isolation").map(str::to_string);
    let tools: Vec<String> = doc
        .get_list("tools")
        .or_else(|| doc.get_list("allowed-tools"))
        .or_else(|| doc.get_list("allowed_tools"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
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

    let item_ref = ItemRef::new(
        HarnessKind::Claude,
        ClaudeItemKind::Agent.as_str(),
        name.clone(),
    );

    let raw = serde_json::json!({
        "name": name,
        "description": description,
        "model": model,
        "effort": effort,
        "isolation": isolation,
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

    let display_name = if description.is_empty() {
        name.clone()
    } else {
        format!("{name} — {description}")
    };

    let item = DiscoveredItem {
        item_ref,
        display_name,
        source: ItemSource {
            path: Some(path.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("loose-agent".into()),
        },
        packaging: None,
        raw,
        capabilities,
        constraints: Vec::new(),
    };

    let mut pending = Vec::new();
    for skill in &preloaded_skills {
        pending.push(AgentPending {
            label: ClaudeEdgeLabel::AgentPreloadsSkill,
            target_kind: ClaudeItemKind::Skill,
            target_id: skill.clone(),
            required: false,
        });
    }
    for mcp in &mcp_refs {
        pending.push(AgentPending {
            label: if mcp_required {
                ClaudeEdgeLabel::ItemRequiresMcp
            } else {
                ClaudeEdgeLabel::ItemSuggestsMcp
            },
            target_kind: ClaudeItemKind::McpServer,
            target_id: mcp.clone(),
            required: mcp_required,
        });
    }

    Ok(Some((item, pending)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{ClaudeDir, DiscoveredRoots};
    use camino::Utf8PathBuf;
    use katachi_core::harness::RosterCatalog;
    use std::fs;
    use tempfile::TempDir;

    fn make_roots(td: &TempDir) -> (Utf8PathBuf, DiscoveredRoots) {
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude/agents")).unwrap();
        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        (root, roots)
    }

    fn scan_alone(roots: &DiscoveredRoots) -> RosterCatalog {
        let mut state = ScanState::new();
        scan_loose_agents(&mut state, roots).unwrap();
        state.finalize()
    }

    #[test]
    fn parses_minimal_agent() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(
            root.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: accessibility reviewer\nmodel: sonnet\n---\nbody",
        )
        .unwrap();
        let catalog = scan_alone(&roots);
        assert_eq!(catalog.items.len(), 1);
        let (ir, item) = catalog.iter_items().next().unwrap();
        assert_eq!(ir.kind, "agent");
        assert_eq!(ir.id, "reviewer");
        assert!(item.capabilities.iter().any(|c| c == "model:sonnet"));
    }

    #[test]
    fn falls_back_to_file_stem_for_name() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(
            root.join(".claude/agents/inspector.md"),
            "# no frontmatter here",
        )
        .unwrap();
        let catalog = scan_alone(&roots);
        let ir = catalog.items.keys().next().unwrap();
        assert_eq!(ir.id, "inspector");
    }

    #[test]
    fn preloaded_skills_produce_edges_when_target_present() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(
            root.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\npreloaded_skills: [axe-runner, wcag-guide]\n---\n",
        )
        .unwrap();
        let mut state = ScanState::new();
        for id in &["axe-runner", "wcag-guide"] {
            state
                .catalog
                .insert_item(DiscoveredItem {
                    item_ref: ItemRef::new(HarnessKind::Claude, "skill", *id),
                    display_name: (*id).into(),
                    source: ItemSource::default(),
                    packaging: None,
                    raw: serde_json::Value::Null,
                    capabilities: Vec::new(),
                    constraints: Vec::new(),
                })
                .unwrap();
        }
        scan_loose_agents(&mut state, &roots).unwrap();
        let catalog = state.finalize();
        let count = catalog
            .edges
            .iter()
            .filter(|e| e.note.as_deref() == Some("agent_preloads_skill"))
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn missing_preloaded_skill_emits_diagnostic() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(
            root.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\npreloaded_skills: [ghost]\n---\n",
        )
        .unwrap();
        let catalog = scan_alone(&roots);
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.agent-missing-target"));
    }

    #[test]
    fn mcp_required_marks_edge_as_required() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(
            root.join(".claude/agents/chrome.md"),
            "---\nname: chrome\nmcp_servers: [chrome-dev]\nmcp_required: true\n---\n",
        )
        .unwrap();
        let mut state = ScanState::new();
        state
            .catalog
            .insert_item(DiscoveredItem {
                item_ref: ItemRef::new(HarnessKind::Claude, "mcp_server", "chrome-dev"),
                display_name: "chrome-dev".into(),
                source: ItemSource::default(),
                packaging: None,
                raw: serde_json::Value::Null,
                capabilities: Vec::new(),
                constraints: Vec::new(),
            })
            .unwrap();
        scan_loose_agents(&mut state, &roots).unwrap();
        let catalog = state.finalize();
        let edge = catalog
            .edges
            .iter()
            .find(|e| e.to.id == "chrome-dev")
            .unwrap();
        assert!(edge.required);
        assert_eq!(edge.note.as_deref(), Some("item_requires_mcp"));
    }

    #[test]
    fn non_markdown_files_ignored() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::write(root.join(".claude/agents/NOTES.txt"), "ignored").unwrap();
        fs::write(
            root.join(".claude/agents/agent.md"),
            "---\nname: agent\n---\n",
        )
        .unwrap();
        let catalog = scan_alone(&roots);
        assert_eq!(catalog.items.len(), 1);
    }
}
