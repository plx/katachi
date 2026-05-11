//! End-to-end discovery tests covering each layout the spec calls out:
//!
//! - loose artifacts only
//! - plugin-packaged artifacts
//! - mixed loose + plugin
//! - missing agent / missing MCP
//! - scope precedence (user + project)
//!
//! SDK projection loss is covered by the unit tests in `sdk.rs` but we
//! also smoke-test a CLI-vs-SDK mismatch here so the catalog-level
//! diagnostic wiring is exercised end-to-end.

use camino::Utf8PathBuf;
use katachi_core::harness::RosterCatalog;
use katachi_core::model::BackendKind;
use katachi_harness_claude::config::ClaudeConfig;
use katachi_harness_claude::discovery::scan_from_roots;
use katachi_harness_claude::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots, ScopedPath};
use katachi_harness_claude::resolve::resolve_roster;
use katachi_harness_claude::roster::ClaudeRoster;
use katachi_harness_claude::sdk;
use std::fs;
use tempfile::TempDir;

fn roots_for(project: &Utf8PathBuf, plugin_root: Option<&Utf8PathBuf>) -> DiscoveredRoots {
    DiscoveredRoots {
        claude_dirs: vec![ClaudeDir {
            path: project.join(".claude"),
            scope: ClaudeScope::Project,
        }],
        top_level_claude_mds: Vec::new(),
        plugin_roots: plugin_root
            .map(|p| {
                vec![ScopedPath {
                    path: p.clone(),
                    scope: ClaudeScope::PluginProject,
                }]
            })
            .unwrap_or_default(),
    }
}

fn write(project: &Utf8PathBuf, rel: &str, contents: &str) {
    let path = project.join(rel);
    fs::create_dir_all(path.parent().unwrap().as_std_path()).unwrap();
    fs::write(path.as_std_path(), contents).unwrap();
}

fn scan(roots: &DiscoveredRoots) -> RosterCatalog {
    scan_from_roots(roots, &ClaudeConfig::default()).unwrap()
}

#[test]
fn loose_artifacts_only() {
    let td = TempDir::new().unwrap();
    let project = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    write(
        &project,
        ".claude/skills/axe/SKILL.md",
        "---\nname: axe\nagent: reviewer\n---\nbody",
    );
    write(
        &project,
        ".claude/agents/reviewer.md",
        "---\nname: reviewer\n---\nbody",
    );
    write(&project, ".claude/CLAUDE.md", "project instructions");
    write(&project, ".claude/rules/style.md", "prefer concise");

    let catalog = scan(&roots_for(&project, None));
    let kinds: std::collections::BTreeSet<_> = catalog
        .iter_items()
        .map(|(ir, _)| ir.kind.clone())
        .collect();
    assert!(kinds.contains("skill"));
    assert!(kinds.contains("agent"));
    assert!(kinds.contains("instruction_source"));
    // skill should be linked to agent via skill_uses_agent
    let edge = catalog
        .iter_edges()
        .find(|e| e.note.as_deref() == Some("skill_uses_agent"))
        .unwrap();
    assert_eq!(edge.from.id, "axe");
    assert_eq!(edge.to.id, "reviewer");
}

#[test]
fn plugin_packaged_artifacts_only() {
    let td = TempDir::new().unwrap();
    let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    let project = root.join("project");
    fs::create_dir_all(project.as_std_path()).unwrap();
    let plugin_root = root.join("plugins");
    let plugin = plugin_root.join("web-a11y");
    fs::create_dir_all(plugin.as_std_path()).unwrap();
    fs::write(
        plugin.join("plugin.json").as_std_path(),
        r#"{"id": "web-a11y", "version": "1.0"}"#,
    )
    .unwrap();
    fs::create_dir_all(plugin.join("skills/axe").as_std_path()).unwrap();
    fs::write(
        plugin.join("skills/axe/SKILL.md").as_std_path(),
        "---\nname: axe\nagent: a11y-agent\n---\n",
    )
    .unwrap();
    fs::create_dir_all(plugin.join("agents").as_std_path()).unwrap();
    fs::write(
        plugin.join("agents/a11y-agent.md").as_std_path(),
        "---\nname: a11y-agent\n---\n",
    )
    .unwrap();

    let catalog = scan(&roots_for(&project, Some(&plugin_root)));
    // Expect: plugin + skill + agent, plus 2 Contains and 1 skill_uses_agent.
    let ids: std::collections::BTreeSet<_> =
        catalog.iter_items().map(|(ir, _)| ir.id.clone()).collect();
    assert!(ids.contains("web-a11y"));
    assert!(ids.contains("axe"));
    assert!(ids.contains("a11y-agent"));
    let contains = catalog
        .iter_edges()
        .filter(|e| e.note.as_deref() == Some("contains"))
        .count();
    assert_eq!(contains, 2);
    assert!(catalog
        .iter_edges()
        .any(|e| e.note.as_deref() == Some("skill_uses_agent")));
}

#[test]
fn mixed_loose_and_plugin() {
    let td = TempDir::new().unwrap();
    let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    let project = root.join("project");
    fs::create_dir_all(project.as_std_path()).unwrap();
    write(
        &project,
        ".claude/skills/loose/SKILL.md",
        "---\nname: loose-skill\nagent: shared-reviewer\n---\n",
    );
    let plugin_root = root.join("plugins");
    let plugin = plugin_root.join("core");
    fs::create_dir_all(plugin.as_std_path()).unwrap();
    fs::write(
        plugin.join("plugin.json").as_std_path(),
        r#"{"id": "core"}"#,
    )
    .unwrap();
    fs::create_dir_all(plugin.join("agents").as_std_path()).unwrap();
    fs::write(
        plugin.join("agents/shared-reviewer.md").as_std_path(),
        "---\nname: shared-reviewer\n---\n",
    )
    .unwrap();

    let catalog = scan(&roots_for(&project, Some(&plugin_root)));
    // Loose skill should wire up to the plugin-packaged agent via the
    // pending-edge mechanism.
    let edge = catalog
        .iter_edges()
        .find(|e| {
            e.note.as_deref() == Some("skill_uses_agent")
                && e.from.id == "loose-skill"
                && e.to.id == "shared-reviewer"
        })
        .unwrap();
    assert!(!edge.required);
}

#[test]
fn missing_agent_reference_produces_diagnostic() {
    let td = TempDir::new().unwrap();
    let project = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    write(
        &project,
        ".claude/skills/axe/SKILL.md",
        "---\nname: axe\nagent: ghost\n---\n",
    );
    let catalog = scan(&roots_for(&project, None));
    assert!(catalog
        .diagnostics
        .iter()
        .any(|d| d.code == "claude.skill-missing-target"));
}

#[test]
fn missing_mcp_reference_produces_diagnostic() {
    let td = TempDir::new().unwrap();
    let project = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    write(
        &project,
        ".claude/skills/axe/SKILL.md",
        "---\nname: axe\nmcp_servers: [ghost-mcp]\nmcp_required: true\n---\n",
    );
    let catalog = scan(&roots_for(&project, None));
    assert!(catalog
        .diagnostics
        .iter()
        .any(|d| d.code == "claude.skill-missing-target"));
}

#[test]
fn scope_precedence_preserves_both() {
    let td = TempDir::new().unwrap();
    let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    let user = root.join("user/.claude");
    let project = root.join("project/.claude");
    fs::create_dir_all(user.as_std_path()).unwrap();
    fs::create_dir_all(project.as_std_path()).unwrap();
    fs::write(user.join("CLAUDE.md").as_std_path(), "user overlay").unwrap();
    fs::write(project.join("CLAUDE.md").as_std_path(), "project overlay").unwrap();
    let roots = DiscoveredRoots {
        claude_dirs: vec![
            ClaudeDir {
                path: user.clone(),
                scope: ClaudeScope::User,
            },
            ClaudeDir {
                path: project.clone(),
                scope: ClaudeScope::Project,
            },
        ],
        top_level_claude_mds: Vec::new(),
        plugin_roots: Vec::new(),
    };
    let catalog = scan(&roots);
    let scopes: std::collections::BTreeSet<_> = catalog
        .iter_items()
        .filter(|(ir, _)| ir.kind == "instruction_source")
        .filter_map(|(_, i)| i.source.scope.clone())
        .collect();
    assert!(scopes.contains("user"));
    assert!(scopes.contains("project"));
}

#[test]
fn sdk_projection_loss_for_file_hook() {
    let td = TempDir::new().unwrap();
    let project = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
    write(
        &project,
        ".claude/settings.json",
        r#"{"hooks": {"PreToolUse": [{"command": "echo"}]}}"#,
    );
    let catalog = scan(&roots_for(&project, None));
    assert!(catalog.iter_items().any(|(ir, _)| ir.kind == "hook_set"));

    // Build a roster that selects the hook and project onto SDK; expect
    // a projection-loss warning.
    let roster = ClaudeRoster::from_toml_str(
        &Utf8PathBuf::from("t.toml"),
        r#"
version = 1
id = "r"
[selection]
hooks = ["project:PreToolUse"]
"#,
    )
    .unwrap();
    let resolved = resolve_roster(&roster, catalog, BackendKind::SdkTs);
    let projection = sdk::project(&resolved, BackendKind::SdkTs);
    assert!(projection
        .diagnostics
        .iter()
        .any(|d| d.code == "claude.sdk-projection-hook"
            || d.code == "claude.sdk-projection-hook-loss"));
}
