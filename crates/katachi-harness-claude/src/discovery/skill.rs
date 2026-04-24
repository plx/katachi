//! Loose skill discovery under `<root>/skills/<name>/SKILL.md`.
//!
//! A loose skill is any `SKILL.md` that lives under a scoped `.claude/`
//! directory (user or project) rather than inside a plugin bundle. Each
//! skill emits a `ClaudeItemKind::Skill` and — when its frontmatter names
//! an `agent` — a semantic edge labelled `skill_uses_agent` from the
//! skill to that agent.

use camino::Utf8Path;
use katachi_core::harness::{DiscoveredItem, EdgeKind, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::discovery::{push_warning, PendingEdge, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::frontmatter;
use crate::item::{ClaudeEdgeLabel, ClaudeItemKind};
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

/// Entry point invoked from [`super::scan_from_roots`].
pub fn scan_loose_skills(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        discover_in_dir(state, dir)?;
    }
    Ok(())
}

fn discover_in_dir(
    state: &mut ScanState,
    dir: &ClaudeDir,
) -> Result<(), ClaudeDiscoveryError> {
    let skills_dir = dir.skills_dir();
    if !skills_dir.exists() {
        return Ok(());
    }
    let read = std::fs::read_dir(skills_dir.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: skills_dir.clone(),
            source,
        }
    })?;
    let mut entries: Vec<camino::Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: skills_dir.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(utf8) = camino::Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        entries.push(utf8);
    }
    entries.sort();
    for entry in entries {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let skill_md = entry.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        match parse_skill_file(&skill_md, dir.scope) {
            Ok(Some((item, pending))) => {
                let item_ref = item.item_ref.clone();
                if let Err(err) = state.catalog.insert_item(item) {
                    push_warning(
                        &mut state.catalog,
                        "claude.duplicate-skill",
                        format!("ignoring duplicate skill at `{}`: {err}", skill_md),
                    );
                } else {
                    for p in pending {
                        state.pending_edges.push(p.into_pending(&item_ref));
                    }
                }
            }
            Ok(None) => {
                push_warning(
                    &mut state.catalog,
                    "claude.skill-missing-name",
                    format!("skill at `{skill_md}` has no `name` frontmatter; skipped"),
                );
            }
            Err(err) => {
                push_warning(
                    &mut state.catalog,
                    "claude.skill-parse-failed",
                    format!("could not parse skill at `{skill_md}`: {err}"),
                );
            }
        }
    }
    Ok(())
}

/// Intermediate pending-edge record used by the skill parser. Each entry
/// is promoted to a real `PendingEdge` once the owning skill's `item_ref`
/// is known.
struct SkillPending {
    label: ClaudeEdgeLabel,
    target_kind: ClaudeItemKind,
    target_id: String,
    required: bool,
}

impl SkillPending {
    fn into_pending(self, from: &ItemRef) -> PendingEdge {
        PendingEdge {
            from: from.clone(),
            target_kind: self.target_kind,
            target_id: self.target_id,
            edge_kind: EdgeKind::Semantic,
            label: self.label,
            required: self.required,
            missing_code: "claude.skill-missing-target".into(),
        }
    }
}

fn parse_skill_file(
    path: &Utf8Path,
    scope: ClaudeScope,
) -> Result<Option<(DiscoveredItem, Vec<SkillPending>)>, ClaudeDiscoveryError> {
    let source = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let doc = frontmatter::parse(path, &source)?;

    let name = match doc.get_str("name") {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            // Fall back to the directory name so every skill is at least
            // addressable.
            let dir_name = path
                .parent()
                .and_then(|p| p.file_name())
                .unwrap_or("")
                .to_string();
            if dir_name.is_empty() {
                return Ok(None);
            }
            dir_name
        }
    };

    let description = doc.get_str("description").unwrap_or("").to_string();
    let agent = doc.get_str("agent").map(|s| s.to_string());
    let mcp_refs: Vec<String> = doc
        .get_list("mcp_servers")
        .or_else(|| doc.get_list("mcp"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let mcp_required = doc.get_bool("mcp_required").unwrap_or(false);
    let allowed_tools: Vec<String> = doc
        .get_list("allowed-tools")
        .or_else(|| doc.get_list("allowed_tools"))
        .map(|v| v.into_iter().map(str::to_string).collect())
        .unwrap_or_default();

    let item_ref = ItemRef::new(HarnessKind::Claude, ClaudeItemKind::Skill.as_str(), name.clone());

    let raw = serde_json::json!({
        "name": name,
        "description": description,
        "agent": agent,
        "mcp_servers": mcp_refs,
        "mcp_required": mcp_required,
        "allowed_tools": allowed_tools,
        "body": doc.body,
        "frontmatter_keys": doc.frontmatter.keys().collect::<Vec<_>>(),
    });

    let mut capabilities = Vec::new();
    for t in &allowed_tools {
        capabilities.push(format!("tool:{t}"));
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
            provenance: Some("loose-skill".into()),
        },
        packaging: None,
        raw,
        capabilities,
        constraints: Vec::new(),
    };

    let mut pending = Vec::new();
    if let Some(agent_id) = agent {
        pending.push(SkillPending {
            label: ClaudeEdgeLabel::SkillUsesAgent,
            target_kind: ClaudeItemKind::Agent,
            target_id: agent_id,
            required: false,
        });
    }
    for mcp in mcp_refs {
        pending.push(SkillPending {
            label: if mcp_required {
                ClaudeEdgeLabel::ItemRequiresMcp
            } else {
                ClaudeEdgeLabel::ItemSuggestsMcp
            },
            target_kind: ClaudeItemKind::McpServer,
            target_id: mcp,
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
        fs::create_dir_all(root.join(".claude/skills")).unwrap();
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
        scan_loose_skills(&mut state, roots).unwrap();
        state.finalize()
    }

    #[test]
    fn parses_minimal_skill() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/greeter")).unwrap();
        fs::write(
            root.join(".claude/skills/greeter/SKILL.md"),
            "---\nname: greeter\ndescription: say hi\n---\nbody",
        )
        .unwrap();

        let catalog = scan_alone(&roots);
        assert_eq!(catalog.items.len(), 1);
        let (ir, item) = catalog.iter_items().next().unwrap();
        assert_eq!(ir.kind, "skill");
        assert_eq!(ir.id, "greeter");
        assert_eq!(item.source.scope.as_deref(), Some("project"));
    }

    #[test]
    fn skill_with_missing_name_uses_dir_name() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/plain")).unwrap();
        fs::write(
            root.join(".claude/skills/plain/SKILL.md"),
            "no frontmatter, just body",
        )
        .unwrap();

        let catalog = scan_alone(&roots);
        assert_eq!(catalog.items.len(), 1);
        assert_eq!(catalog.items.keys().next().unwrap().id, "plain");
    }

    #[test]
    fn skill_with_agent_emits_diagnostic_when_missing() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/axe")).unwrap();
        fs::write(
            root.join(".claude/skills/axe/SKILL.md"),
            "---\nname: axe\nagent: reviewer\n---\n",
        )
        .unwrap();

        let catalog = scan_alone(&roots);
        assert_eq!(catalog.items.len(), 1);
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.skill-missing-target"));
    }

    #[test]
    fn skill_agent_edge_emitted_when_target_present() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/axe")).unwrap();
        fs::write(
            root.join(".claude/skills/axe/SKILL.md"),
            "---\nname: axe\nagent: reviewer\n---\n",
        )
        .unwrap();

        let mut state = ScanState::new();
        // Pre-populate the reviewer agent so the skill can wire up to it.
        state
            .catalog
            .insert_item(DiscoveredItem {
                item_ref: ItemRef::new(HarnessKind::Claude, "agent", "reviewer"),
                display_name: "reviewer".into(),
                source: ItemSource::default(),
                packaging: None,
                raw: serde_json::Value::Null,
                capabilities: Vec::new(),
                constraints: Vec::new(),
            })
            .unwrap();
        scan_loose_skills(&mut state, &roots).unwrap();
        let catalog = state.finalize();
        assert_eq!(catalog.edges.len(), 1);
        let e = &catalog.edges[0];
        assert_eq!(e.from.id, "axe");
        assert_eq!(e.to.id, "reviewer");
        assert_eq!(e.note.as_deref(), Some("skill_uses_agent"));
    }

    #[test]
    fn malformed_skill_leaves_diagnostic_not_error() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/bad")).unwrap();
        fs::write(
            root.join(".claude/skills/bad/SKILL.md"),
            "---\njust-a-word-no-colon\n---\n",
        )
        .unwrap();

        let catalog = scan_alone(&roots);
        assert!(catalog.items.is_empty());
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.skill-parse-failed"));
    }

    #[test]
    fn mcp_required_produces_requires_edge_diagnostic() {
        let td = TempDir::new().unwrap();
        let (root, roots) = make_roots(&td);
        fs::create_dir_all(root.join(".claude/skills/axe")).unwrap();
        fs::write(
            root.join(".claude/skills/axe/SKILL.md"),
            "---\nname: axe\nmcp_servers: [chrome]\nmcp_required: true\n---\n",
        )
        .unwrap();

        let catalog = scan_alone(&roots);
        let (_, item) = catalog.iter_items().next().unwrap();
        assert!(item.raw["mcp_required"].as_bool().unwrap_or(false));
    }
}
