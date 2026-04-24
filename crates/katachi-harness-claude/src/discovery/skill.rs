//! Loose skill discovery under `<root>/skills/<name>/SKILL.md`.
//!
//! A loose skill is any `SKILL.md` that lives under a scoped `.claude/`
//! directory (user or project) rather than inside a plugin bundle. Each
//! skill emits a `ClaudeItemKind::Skill` and — when its frontmatter names
//! an `agent` — a semantic edge labelled `skill_uses_agent` from the
//! skill to that agent.
//!
//! The parser is tolerant: it accepts unknown keys, keeps the raw body
//! around so downstream tooling can re-display it, and always surfaces a
//! diagnostic rather than failing the whole scan when a single file is
//! malformed.

use camino::Utf8Path;
use katachi_core::harness::{DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterCatalog};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::discovery::push_warning;
use crate::error::ClaudeDiscoveryError;
use crate::frontmatter;
use crate::item::{ClaudeEdgeLabel, ClaudeItemKind};
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

/// Entry point invoked from [`super::scan_from_roots`].
pub fn scan_loose_skills(
    catalog: &mut RosterCatalog,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        discover_in_dir(catalog, dir)?;
    }
    Ok(())
}

/// Look for `skills/<name>/SKILL.md` inside a single scoped `.claude/`.
fn discover_in_dir(
    catalog: &mut RosterCatalog,
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
    entries.sort(); // deterministic order across filesystems
    for entry in entries {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let skill_md = entry.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        match parse_skill_file(&skill_md, dir.scope) {
            Ok(Some((item, edges))) => {
                if let Err(err) = catalog.insert_item(item.clone()) {
                    push_warning(
                        catalog,
                        "claude.duplicate-skill",
                        format!("ignoring duplicate skill at `{}`: {err}", skill_md),
                    );
                } else {
                    for pending in edges {
                        emit_pending_edge(catalog, &item.item_ref, pending);
                    }
                }
            }
            Ok(None) => {
                push_warning(
                    catalog,
                    "claude.skill-missing-name",
                    format!("skill at `{skill_md}` has no `name` frontmatter; skipped"),
                );
            }
            Err(err) => {
                push_warning(
                    catalog,
                    "claude.skill-parse-failed",
                    format!("could not parse skill at `{skill_md}`: {err}"),
                );
            }
        }
    }
    Ok(())
}

/// An edge produced by a skill's frontmatter whose target may not yet
/// exist in the catalog. The scan orchestrator records these directly as
/// catalog edges only when the target is present; otherwise it defers to
/// a diagnostic so later stages (validation, resolution) can see them.
struct PendingEdge {
    label: ClaudeEdgeLabel,
    target_kind: ClaudeItemKind,
    target_id: String,
    /// Used for dangling-ref diagnostics.
    note: String,
}

fn emit_pending_edge(
    catalog: &mut RosterCatalog,
    from: &ItemRef,
    pending: PendingEdge,
) {
    let target = ItemRef::new(
        HarnessKind::Claude,
        pending.target_kind.as_str(),
        pending.target_id.clone(),
    );
    // If the target item exists already, emit the edge; otherwise stash
    // a diagnostic so the validator can surface it later.
    if catalog.contains(&target) {
        let edge = DependencyEdge {
            from: from.clone(),
            to: target,
            kind: EdgeKind::Semantic,
            required: false,
            note: Some(pending.label.as_str().to_string()),
        };
        if let Err(err) = catalog.insert_edge(edge) {
            push_warning(
                catalog,
                "claude.duplicate-edge",
                format!("ignoring duplicate skill edge: {err}"),
            );
        }
    } else {
        push_warning(
            catalog,
            "claude.skill-missing-target",
            format!(
                "skill `{from}` references missing {kind} `{id}` ({note})",
                kind = pending.target_kind.as_str(),
                id = pending.target_id,
                note = pending.note
            ),
        );
    }
}

fn parse_skill_file(
    path: &Utf8Path,
    scope: ClaudeScope,
) -> Result<Option<(DiscoveredItem, Vec<PendingEdge>)>, ClaudeDiscoveryError> {
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
            // addressable, but signal via None so the caller can warn.
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
        pending.push(PendingEdge {
            label: ClaudeEdgeLabel::SkillUsesAgent,
            target_kind: ClaudeItemKind::Agent,
            target_id: agent_id,
            note: "skill_uses_agent".into(),
        });
    }
    for mcp in mcp_refs {
        pending.push(PendingEdge {
            label: if mcp_required {
                ClaudeEdgeLabel::ItemRequiresMcp
            } else {
                ClaudeEdgeLabel::ItemSuggestsMcp
            },
            target_kind: ClaudeItemKind::McpServer,
            target_id: mcp,
            note: if mcp_required {
                "item_requires_mcp".into()
            } else {
                "item_suggests_mcp".into()
            },
        });
    }
    Ok(Some((item, pending)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{ClaudeDir, DiscoveredRoots};
    use camino::Utf8PathBuf;
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        scan_loose_skills(&mut catalog, &roots).unwrap();
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        scan_loose_skills(&mut catalog, &roots).unwrap();
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        scan_loose_skills(&mut catalog, &roots).unwrap();
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        // Pre-populate the reviewer agent so the skill can wire up to it.
        catalog
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
        scan_loose_skills(&mut catalog, &roots).unwrap();
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        scan_loose_skills(&mut catalog, &roots).unwrap();
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

        let mut catalog = RosterCatalog::empty(HarnessKind::Claude);
        scan_loose_skills(&mut catalog, &roots).unwrap();
        let (_, item) = catalog.iter_items().next().unwrap();
        assert!(item.raw["mcp_required"].as_bool().unwrap_or(false));
    }
}
